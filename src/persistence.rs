use crate::errors::Errors;
use crate::options::DharmaOpts;
use crate::sparse_index::{SparseIndex, TableAddress};
use crate::storage::block::Value;
use crate::storage::sorted_string_table_reader::{SSTableReader, SSTableValue};
use crate::storage::sorted_string_table_writer::write_sstable;
use crate::traits::{ResourceKey, ResourceValue};
use std::path::PathBuf;
use std::cmp::Ordering;
use crate::storage::write_ahead_log::WriteAheadLog;

/// Encapsulates all functionality that involves reading
/// and writing to File System.
pub struct Persistence<K: ResourceKey> {
    options: DharmaOpts,
    index: SparseIndex<K>,
    log: WriteAheadLog,
}

impl<K> Persistence<K>
where
    K: ResourceKey,
{
    pub fn create<V: ResourceValue>(options: DharmaOpts) -> Result<Persistence<K>, Errors> {
        // try to create write ahead log
        let log_result = WriteAheadLog::new(options.clone());
        if log_result.is_ok() {
            // read all SSTables and create the sparse index
            let sstable_paths = SSTableReader::get_valid_table_paths(&options.path)?;
            // read through each SSTable and create the sparse index on startup
            let mut index = SparseIndex::new();
            for path in sstable_paths {
                let load_result =
                    Persistence::populate_index_from_path::<V>(&options, &path, &mut index);
                if load_result.is_err() {
                    return Err(Errors::DB_INDEX_INITIALIZATION_FAILED);
                }
            }
            Ok(Persistence { log: log_result.unwrap(), options, index })
        }
        Err(log_result.err().unwrap())
    }

    pub fn get<V: ResourceValue>(&mut self, key: &K) -> Result<Option<V>, Errors> {
        // read SSTables and return the value is present
        let maybe_address = self.index.get_nearest_address(key);
        if maybe_address.is_some() {
            let address = maybe_address.unwrap();
            let mut reader = SSTableReader::from(&address.path, self.options.block_size_in_bytes)?;
            // try to find the value in the sstable
            let seek_result = reader.seek_closest(address.offset);
            // if seek offset is invalid then return errror
            // this should never happen as long as SSTables and Sparse Index are in sync
            if seek_result.is_ok() {
                while reader.has_next() {
                    let sstable_value = reader.read();
                    let record =
                        bincode::deserialize::<Value<K, V>>(&sstable_value.data).unwrap();
                    match record.key.cmp(key) {
                        Ordering::Less => {
                            reader.next();
                        }
                        Ordering::Equal => {
                            return Ok(Some(record.value));
                        }
                        Ordering::Greater => {
                            return Ok(None);
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    pub fn insert<V: ResourceValue>(&mut self, key: K, value: V) -> Result<(), Errors> {
        let log_write_result = self.log.append(key.clone(), value.clone());
        if log_write_result.is_ok() {
            return Ok(());
        }
        Err(Errors::DB_WRITE_FAILED)
    }

    pub fn flush<V: ResourceValue>(&mut self, values: &Vec<(K, V)>) -> Result<(), Errors> {
        // get the existing SSTable paths
        let paths = SSTableReader::get_valid_table_paths(&self.options.path)?;
        let flush_result = write_sstable(&self.options, values, paths.len());
        if flush_result.is_ok() {
            let new_sstable_path = flush_result.unwrap();
            //TODO: clear WAL log here
            let index_update_result = Persistence::populate_index_from_path::<V>(
                &self.options,
                &new_sstable_path,
                &mut self.index,
            );
            if index_update_result.is_err() {
                return Err(Errors::DB_INDEX_UPDATE_FAILED);
            }
            return Ok(());
        }
        Err(Errors::SSTABLE_CREATION_FAILED)
    }

    pub fn delete(&mut self, key: &K) -> Result<(), Errors> {
        // add delete marker to Write Ahead Log
        unimplemented!()
    }

    fn populate_index_from_path<V: ResourceValue>(
        options: &DharmaOpts,
        path: &PathBuf,
        index: &mut SparseIndex<K>,
    ) -> Result<(), Errors> {
        let mut counter = 0;
        let maybe_reader = SSTableReader::from(path, options.block_size_in_bytes);
        if maybe_reader.is_ok() {
            let mut reader = maybe_reader.unwrap();
            while reader.has_next() {
                if counter % options.sparse_index_sampling_rate == 0 {
                    let sstable_value: SSTableValue = reader.read();
                    let record: Value<K, V> =
                        bincode::deserialize(sstable_value.data.as_slice()).unwrap();
                    let key = record.key;
                    let offset = sstable_value.offset;
                    let address = TableAddress::new(path, offset);
                    index.update(key.clone(), address);
                }
                counter += 1;
                reader.next();
            }
            return Ok(());
        }
        Err(Errors::SSTABLE_READ_FAILED)
    }
}
