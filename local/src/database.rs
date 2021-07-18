use std::{
    any::Any, borrow::Cow, collections::HashMap, marker::PhantomData, path::Path, sync::Arc, u8,
};

use async_trait::async_trait;
#[cfg(feature = "keyvalue")]
use bonsaidb_core::kv::{KeyOperation, Kv, Output};
use bonsaidb_core::{
    connection::{AccessPolicy, Connection, QueryKey, ServerConnection},
    document::{Document, Header, KeyId},
    limits::{LIST_TRANSACTIONS_DEFAULT_RESULT_COUNT, LIST_TRANSACTIONS_MAX_RESULTS},
    networking::{self},
    permissions::Permissions,
    schema::{
        self,
        view::{self, map},
        CollectionName, Key, Map, MappedDocument, MappedValue, Schema, Schematic, ViewName,
    },
    transaction::{
        self, ChangedDocument, Command, Executed, Operation, OperationResult, Transaction,
    },
};
use itertools::Itertools;
use sled::{
    transaction::{ConflictableTransactionError, TransactionError, TransactionalTree},
    IVec, Transactional, Tree,
};

use crate::{
    config::Configuration,
    error::Error,
    open_trees::OpenTrees,
    storage::OpenDatabase,
    vault::Vault,
    views::{
        mapper::{self, ViewEntryCollection},
        view_document_map_tree_name, view_entries_tree_name, view_invalidated_docs_tree_name,
        view_omitted_docs_tree_name, ViewEntry,
    },
    Storage,
};
#[cfg(feature = "keyvalue")]
pub mod kv;

#[cfg(feature = "pubsub")]
pub mod pubsub;

/// A local, file-based database.
#[derive(Debug)]
pub struct Database<DB> {
    pub(crate) data: Arc<Data<DB>>,
}

#[derive(Debug)]
pub struct Data<DB> {
    pub name: Arc<Cow<'static, str>>,
    pub(crate) storage: Storage,
    pub(crate) schema: Arc<Schematic>,
    pub(crate) effective_permissions: Option<Permissions>,
    _schema: PhantomData<DB>,
}

impl<DB> Clone for Database<DB> {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
        }
    }
}

impl<DB> Database<DB>
where
    DB: Schema,
{
    /// Opens a local file as a bonsaidb.
    pub(crate) async fn new<S: Into<Cow<'static, str>> + Send>(
        name: S,
        storage: Storage,
    ) -> Result<Self, Error> {
        let schema = Arc::new(DB::schematic()?);
        let db = Self {
            data: Arc::new(Data {
                name: Arc::new(name.into()),
                storage,
                schema,
                effective_permissions: None,
                _schema: PhantomData::default(),
            }),
        };

        if db.data.storage.check_view_integrity_on_database_open() {
            for view in db.data.schema.views() {
                db.data
                    .storage
                    .tasks()
                    .spawn_integrity_check(view, &db)
                    .await?;
            }
        }

        Ok(db)
    }

    /// Returns a clone with `effective_permissions`. Replaces any previously applied permissions.
    ///
    /// # Unstable
    ///
    /// See [this issue](https://github.com/khonsulabs/bonsaidb/issues/68).
    #[doc(hidden)]
    #[must_use]
    pub fn with_effective_permissions(&self, effective_permissions: Permissions) -> Self {
        Self {
            data: Arc::new(Data {
                name: self.data.name.clone(),
                storage: self.data.storage.clone(),
                schema: self.data.schema.clone(),
                effective_permissions: Some(effective_permissions),
                _schema: PhantomData::default(),
            }),
        }
    }

    /// Returns the name of the database.
    #[must_use]
    pub fn name(&self) -> &str {
        self.data.name.as_ref()
    }

    /// Creates a `Storage` with a single-database named "default" with its data stored at `path`.
    pub async fn open_local<P: AsRef<Path> + Send>(
        path: P,
        configuration: Configuration,
    ) -> Result<Self, Error> {
        let storage = Storage::open_local(path, configuration).await?;
        storage.register_schema::<DB>().await?;

        match storage.create_database::<DB>("default").await {
            Ok(_) | Err(bonsaidb_core::Error::DatabaseNameAlreadyTaken(_)) => {}
            err => err?,
        }

        Ok(storage.database("default").await?)
    }

    /// Returns the [`Storage`] that this database belongs to.
    #[must_use]
    pub fn storage(&self) -> &'_ Storage {
        &self.data.storage
    }

    /// Returns the [`Schematic`] for `DB`.
    #[must_use]
    pub fn schematic(&self) -> &'_ Schematic {
        &self.data.schema
    }

    pub(crate) fn sled(&self) -> &'_ sled::Db {
        self.data.storage.sled()
    }

    async fn for_each_in_view<
        F: FnMut(ViewEntryCollection) -> Result<(), bonsaidb_core::Error> + Send + Sync,
    >(
        &self,
        view: &dyn view::Serialized,
        key: Option<QueryKey<Vec<u8>>>,
        access_policy: AccessPolicy,
        mut callback: F,
    ) -> Result<(), bonsaidb_core::Error> {
        if matches!(access_policy, AccessPolicy::UpdateBefore) {
            self.data
                .storage
                .tasks()
                .update_view_if_needed(view, self)
                .await?;
        }

        let view_entries = self
            .sled()
            .open_tree(view_entries_tree_name(&self.data.name, &view.view_name()?))
            .map_err(Error::Sled)?;

        {
            let iterator = self.create_view_iterator(&view_entries, key, view)?;
            let entries = iterator.collect::<Result<Vec<_>, Error>>()?;

            for entry in entries {
                callback(entry)?;
            }
        }

        if matches!(access_policy, AccessPolicy::UpdateAfter) {
            let db = self.clone();
            let view_name = view.view_name()?;
            tokio::task::spawn(async move {
                let view = db
                    .data
                    .schema
                    .view_by_name(&view_name)
                    .expect("query made with view that isn't registered with this database");
                db.data
                    .storage
                    .tasks()
                    .update_view_if_needed(view, &db)
                    .await
            });
        }

        Ok(())
    }

    async fn for_each_view_entry<
        V: schema::View,
        F: FnMut(ViewEntryCollection) -> Result<(), bonsaidb_core::Error> + Send + Sync,
    >(
        &self,
        key: Option<QueryKey<V::Key>>,
        access_policy: AccessPolicy,
        callback: F,
    ) -> Result<(), bonsaidb_core::Error> {
        let view = self
            .data
            .schema
            .view::<V>()
            .expect("query made with view that isn't registered with this database");

        self.for_each_in_view(
            view,
            key.map(|key| key.serialized()).transpose()?,
            access_policy,
            callback,
        )
        .await
    }

    async fn get_from_collection_id(
        &self,
        id: u64,
        collection: &CollectionName,
    ) -> Result<Option<Document<'static>>, bonsaidb_core::Error> {
        let task_self = self.clone();
        let collection = collection.clone();
        tokio::task::spawn_blocking(move || {
            let tree = task_self
                .data
                .storage
                .sled()
                .open_tree(document_tree_name(&task_self.data.name, &collection))
                .map_err(Error::from)?;
            if let Some(vec) = tree
                .get(
                    id.as_big_endian_bytes()
                        .map_err(view::Error::KeySerialization)?,
                )
                .map_err(Error::from)?
            {
                Ok(Some(task_self.deserialize_document(&vec)?.to_owned()))
            } else {
                Ok(None)
            }
        })
        .await
        .unwrap()
    }

    async fn get_multiple_from_collection_id(
        &self,
        ids: &[u64],
        collection: &CollectionName,
    ) -> Result<Vec<Document<'static>>, bonsaidb_core::Error> {
        let task_self = self.clone();
        let ids = ids.to_vec();
        let collection = collection.clone();
        tokio::task::spawn_blocking(move || {
            let tree = task_self
                .data
                .storage
                .sled()
                .open_tree(document_tree_name(&task_self.data.name, &collection))
                .map_err(Error::from)?;
            let mut found_docs = Vec::new();
            for id in ids {
                if let Some(vec) = tree
                    .get(
                        id.as_big_endian_bytes()
                            .map_err(view::Error::KeySerialization)?,
                    )
                    .map_err(Error::from)?
                {
                    found_docs.push(task_self.deserialize_document(&vec)?.to_owned());
                }
            }

            Ok(found_docs)
        })
        .await
        .unwrap()
    }

    async fn reduce_in_view(
        &self,
        view_name: &ViewName,
        key: Option<QueryKey<Vec<u8>>>,
        access_policy: AccessPolicy,
    ) -> Result<Vec<u8>, bonsaidb_core::Error> {
        let view = self
            .data
            .schema
            .view_by_name(view_name)
            .ok_or(bonsaidb_core::Error::CollectionNotFound)?;
        let mut mappings = self
            .grouped_reduce_in_view(view_name, key, access_policy)
            .await?;

        let result = if mappings.len() == 1 {
            mappings.pop().unwrap().value
        } else {
            view.reduce(
                &mappings
                    .iter()
                    .map(|map| (map.key.as_ref(), map.value.as_ref()))
                    .collect::<Vec<_>>(),
                true,
            )
            .map_err(Error::View)?
        };

        Ok(result)
    }

    async fn grouped_reduce_in_view(
        &self,
        view_name: &ViewName,
        key: Option<QueryKey<Vec<u8>>>,
        access_policy: AccessPolicy,
    ) -> Result<Vec<MappedValue<Vec<u8>, Vec<u8>>>, bonsaidb_core::Error> {
        let view = self
            .data
            .schema
            .view_by_name(view_name)
            .ok_or(bonsaidb_core::Error::CollectionNotFound)?;
        let mut mappings = Vec::new();
        self.for_each_in_view(view, key, access_policy, |entry| {
            let entry = ViewEntry::from(entry);
            mappings.push(MappedValue {
                key: entry.key,
                value: entry.reduced_value,
            });
            Ok(())
        })
        .await?;

        Ok(mappings)
    }

    pub(crate) fn deserialize_document<'a>(
        &self,
        bytes: &'a [u8],
    ) -> Result<Document<'a>, bonsaidb_core::Error> {
        let mut document = bincode::deserialize::<Document<'_>>(bytes).map_err(Error::from)?;
        if let Some(_decryption_key) = &document.header.encryption_key {
            let decrypted_contents = self
                .storage()
                .vault()
                .decrypt_payload(&document.contents, self.data.effective_permissions.as_ref())?;
            document.contents = Cow::Owned(decrypted_contents);
        }
        Ok(document)
    }

    fn serialize_document(&self, document: &Document<'_>) -> Result<Vec<u8>, bonsaidb_core::Error> {
        if let Some(encryption_key) = &document.header.encryption_key {
            let encrypted_contents = self.storage().vault().encrypt_payload(
                encryption_key,
                &document.contents,
                self.data.effective_permissions.as_ref(),
            )?;
            bincode::serialize(&Document {
                header: document.header.clone(),
                contents: Cow::from(encrypted_contents),
            })
        } else {
            bincode::serialize(document)
        }
        .map_err(Error::from)
        .map_err(bonsaidb_core::Error::from)
    }

    fn execute_operation(
        &self,
        operation: &Operation<'_>,
        trees: &[TransactionalTree],
        tree_index_map: &HashMap<String, usize>,
    ) -> Result<OperationResult, ConflictableTransactionError<bonsaidb_core::Error>> {
        match &operation.command {
            Command::Insert {
                contents,
                encryption_key,
            } => self.execute_insert(
                operation,
                trees,
                tree_index_map,
                contents.clone(),
                encryption_key
                    .as_ref()
                    .or_else(|| self.collection_encryption_key(&operation.collection))
                    .cloned(),
            ),
            Command::Update { header, contents } => {
                self.execute_update(operation, trees, tree_index_map, header, contents.clone())
            }
            Command::Delete { header } => {
                self.execute_delete(operation, trees, tree_index_map, header)
            }
        }
    }

    fn execute_update(
        &self,
        operation: &Operation<'_>,
        trees: &[TransactionalTree],
        tree_index_map: &HashMap<String, usize>,
        header: &Header,
        contents: Cow<'_, [u8]>,
    ) -> Result<OperationResult, ConflictableTransactionError<bonsaidb_core::Error>> {
        let documents =
            &trees[tree_index_map[&document_tree_name(self.name(), &operation.collection)]];
        let document_id = IVec::from(header.id.as_big_endian_bytes().unwrap().as_ref());
        if let Some(vec) = documents.get(&document_id)? {
            let doc = self
                .deserialize_document(&vec)
                .map_err(ConflictableTransactionError::Abort)?;
            if doc.header.revision == header.revision {
                if let Some(mut updated_doc) = doc.create_new_revision(contents) {
                    // Copy the encryption key if it's been updated.
                    if updated_doc.header.encryption_key != header.encryption_key {
                        updated_doc.header.to_mut().encryption_key = header.encryption_key.clone();
                    }

                    self.save_doc(documents, &updated_doc)?;

                    self.update_unique_views(
                        &document_id,
                        operation,
                        documents,
                        trees,
                        tree_index_map,
                    )?;

                    Ok(OperationResult::DocumentUpdated {
                        collection: operation.collection.clone(),
                        header: updated_doc.header.as_ref().clone(),
                    })
                } else {
                    // If no new revision was made, it means an attempt to
                    // save a document with the same contents was made.
                    // We'll return a success but not actually give a new
                    // version
                    Ok(OperationResult::DocumentUpdated {
                        collection: operation.collection.clone(),
                        header: doc.header.as_ref().clone(),
                    })
                }
            } else {
                Err(ConflictableTransactionError::Abort(
                    bonsaidb_core::Error::DocumentConflict(operation.collection.clone(), header.id),
                ))
            }
        } else {
            Err(ConflictableTransactionError::Abort(
                bonsaidb_core::Error::DocumentNotFound(operation.collection.clone(), header.id),
            ))
        }
    }

    fn execute_insert(
        &self,
        operation: &Operation<'_>,
        trees: &[TransactionalTree],
        tree_index_map: &HashMap<String, usize>,
        contents: Cow<'_, [u8]>,
        encryption_key: Option<KeyId>,
    ) -> Result<OperationResult, ConflictableTransactionError<bonsaidb_core::Error>> {
        let documents =
            &trees[tree_index_map[&document_tree_name(self.name(), &operation.collection)]];
        let doc = Document::new(documents.generate_id()?, contents, encryption_key);
        self.save_doc(documents, &doc)?;
        let serialized: Vec<u8> = self
            .serialize_document(&doc)
            .map_err(ConflictableTransactionError::Abort)?;
        let document_id = IVec::from(doc.header.id.as_big_endian_bytes().unwrap().as_ref());
        documents.insert(document_id.as_ref(), serialized)?;

        self.update_unique_views(&document_id, operation, documents, trees, tree_index_map)?;

        Ok(OperationResult::DocumentUpdated {
            collection: operation.collection.clone(),
            header: doc.header.as_ref().clone(),
        })
    }

    fn execute_delete(
        &self,
        operation: &Operation<'_>,
        trees: &[TransactionalTree],
        tree_index_map: &HashMap<String, usize>,
        header: &Header,
    ) -> Result<OperationResult, ConflictableTransactionError<bonsaidb_core::Error>> {
        let documents =
            &trees[tree_index_map[&document_tree_name(self.name(), &operation.collection)]];
        let document_id = header.id.as_big_endian_bytes().unwrap();
        if let Some(vec) = documents.get(&document_id)? {
            let doc = self
                .deserialize_document(&vec)
                .map_err(ConflictableTransactionError::Abort)?;
            if doc.header.as_ref() == header {
                documents.remove(document_id.as_ref())?;

                self.update_unique_views(
                    &IVec::from(document_id.as_ref()),
                    operation,
                    documents,
                    trees,
                    tree_index_map,
                )?;

                Ok(OperationResult::DocumentDeleted {
                    collection: operation.collection.clone(),
                    id: header.id,
                })
            } else {
                Err(ConflictableTransactionError::Abort(
                    bonsaidb_core::Error::DocumentConflict(operation.collection.clone(), header.id),
                ))
            }
        } else {
            Err(ConflictableTransactionError::Abort(
                bonsaidb_core::Error::DocumentNotFound(operation.collection.clone(), header.id),
            ))
        }
    }

    fn update_unique_views(
        &self,
        document_id: &IVec,
        operation: &Operation<'_>,
        documents: &TransactionalTree,
        trees: &[TransactionalTree],
        tree_index_map: &HashMap<String, usize>,
    ) -> Result<(), ConflictableTransactionError<bonsaidb_core::Error>> {
        if let Some(unique_views) = self
            .data
            .schema
            .unique_views_in_collection(&operation.collection)
        {
            for view in unique_views {
                let name = view
                    .view_name()
                    .map_err(bonsaidb_core::Error::from)
                    .map_err(ConflictableTransactionError::Abort)?;
                mapper::DocumentRequest {
                    database: self,
                    document_id,
                    map_request: &mapper::Map {
                        database: self.data.name.clone(),
                        collection: operation.collection.clone(),
                        view_name: name.clone(),
                    },
                    document_map: &trees
                        [tree_index_map[&view_document_map_tree_name(self.name(), &name)]],
                    documents,
                    omitted_entries: &trees
                        [tree_index_map[&view_omitted_docs_tree_name(self.name(), &name)]],
                    view_entries: &trees
                        [tree_index_map[&view_entries_tree_name(self.name(), &name)]],
                    view,
                }
                .map()?;
            }
        }

        Ok(())
    }

    fn save_doc(
        &self,
        tree: &TransactionalTree,
        doc: &Document<'_>,
    ) -> Result<(), ConflictableTransactionError<bonsaidb_core::Error>> {
        let serialized: Vec<u8> = self
            .serialize_document(doc)
            .map_err(ConflictableTransactionError::Abort)?;
        tree.insert(
            doc.header.id.as_big_endian_bytes().unwrap().as_ref(),
            serialized,
        )?;
        Ok(())
    }
    fn create_view_iterator<'a, K: Key + 'a>(
        &'a self,
        view_entries: &'a Tree,
        key: Option<QueryKey<K>>,
        view: &'a dyn view::Serialized,
    ) -> Result<ViewEntryIterator<'a>, view::Error> {
        if let Some(key) = key {
            match key {
                QueryKey::Range(range) => {
                    if view.keys_are_encryptable() {
                        Err(view::Error::RangeQueryNotSupported)
                    } else {
                        let start = range
                            .start
                            .as_big_endian_bytes()
                            .map_err(view::Error::KeySerialization)?;
                        let end = range
                            .end
                            .as_big_endian_bytes()
                            .map_err(view::Error::KeySerialization)?;
                        Ok(Box::new(ViewEntryCollectionIterator {
                            iterator: Box::new(view_entries.range(start..end)),
                            permissions: self.data.effective_permissions.as_ref(),
                            vault: self.storage().vault(),
                        }))
                    }
                }
                QueryKey::Matches(key) => {
                    let key = key
                        .as_big_endian_bytes()
                        .map_err(view::Error::KeySerialization)?
                        .to_vec();

                    Ok(Box::new(KeyViewEntryIterator {
                        tree: view_entries,
                        keys: vec![key].into_iter(),
                        view,
                        encrypt_by_default: self.view_encryption_key(view).is_some(),
                        permissions: self.data.effective_permissions.as_ref(),
                        vault: self.storage().vault(),
                    }))
                }
                QueryKey::Multiple(list) => {
                    let list = list
                        .into_iter()
                        .map(|key| {
                            key.as_big_endian_bytes()
                                .map(|bytes| bytes.to_vec())
                                .map_err(view::Error::KeySerialization)
                        })
                        .collect::<Result<Vec<_>, _>>()?;

                    Ok(Box::new(KeyViewEntryIterator {
                        tree: view_entries,
                        keys: list.into_iter(),
                        view,
                        encrypt_by_default: self.view_encryption_key(view).is_some(),
                        permissions: self.data.effective_permissions.as_ref(),
                        vault: self.storage().vault(),
                    }))
                }
            }
        } else {
            Ok(Box::new(ViewEntryCollectionIterator {
                iterator: Box::new(view_entries.iter()),
                permissions: self.data.effective_permissions.as_ref(),
                vault: self.storage().vault(),
            }))
        }
    }

    pub(crate) fn collection_encryption_key(&self, collection: &CollectionName) -> Option<&KeyId> {
        self.schematic()
            .encryption_key_for_collection(collection)
            .or_else(|| self.storage().default_encryption_key())
    }

    pub(crate) fn view_encryption_key(&self, view: &dyn view::Serialized) -> Option<&KeyId> {
        view.collection()
            .ok()
            .and_then(|name| self.collection_encryption_key(&name))
    }
}

#[async_trait]
impl<'a, DB> Connection for Database<DB>
where
    DB: Schema,
{
    async fn apply_transaction(
        &self,
        transaction: Transaction<'static>,
    ) -> Result<Vec<OperationResult>, bonsaidb_core::Error> {
        let task_self = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut open_trees = OpenTrees::default();
            let transaction_tree_name = transaction_tree_name(task_self.name());
            open_trees
                .open_tree(task_self.sled(), &transaction_tree_name)
                .map_err(Error::from)?;
            for op in &transaction.operations {
                if !task_self.data.schema.contains_collection_id(&op.collection) {
                    return Err(bonsaidb_core::Error::CollectionNotFound);
                }

                match &op.command {
                    Command::Update { .. } | Command::Insert { .. } | Command::Delete { .. } => {
                        open_trees
                            .open_trees_for_document_change(
                                task_self.sled(),
                                task_self.name(),
                                &op.collection,
                                &task_self.data.schema,
                            )
                            .map_err(Error::from)?;
                    }
                }
            }

            match open_trees.trees.transaction(|trees| {
                let mut results = Vec::new();
                let mut changed_documents = Vec::new();
                for op in &transaction.operations {
                    let result =
                        task_self.execute_operation(op, trees, &open_trees.trees_index_by_name)?;

                    match &result {
                        OperationResult::DocumentUpdated { header, collection } => {
                            changed_documents.push(ChangedDocument {
                                collection: collection.clone(),
                                id: header.id,
                                deleted: false,
                            });
                        }
                        OperationResult::DocumentDeleted { id, collection } => changed_documents
                            .push(ChangedDocument {
                                collection: collection.clone(),
                                id: *id,
                                deleted: true,
                            }),
                        OperationResult::Success => {}
                    }
                    results.push(result);
                }

                // Insert invalidations for each record changed
                for (collection, changed_documents) in &changed_documents
                    .iter()
                    .group_by(|doc| doc.collection.clone())
                {
                    if let Some(views) = task_self.data.schema.views_in_collection(&collection) {
                        let changed_documents = changed_documents.collect::<Vec<_>>();
                        for view in views {
                            if !view.unique() {
                                let view_name = view
                                    .view_name()
                                    .map_err(bonsaidb_core::Error::from)
                                    .map_err(ConflictableTransactionError::Abort)?;
                                for changed_document in &changed_documents {
                                    let invalidated_docs = &trees[open_trees.trees_index_by_name
                                        [&view_invalidated_docs_tree_name(
                                            task_self.name(),
                                            &view_name,
                                        )]];
                                    invalidated_docs.insert(
                                        changed_document.id.as_big_endian_bytes().unwrap().as_ref(),
                                        IVec::default(),
                                    )?;
                                }
                            }
                        }
                    }
                }

                // Save a record of the transaction we just completed.
                let tree = &trees[open_trees.trees_index_by_name[&transaction_tree_name]];
                let executed = transaction::Executed {
                    id: tree.generate_id()?,
                    changed_documents: Cow::from(changed_documents),
                };
                let serialized: Vec<u8> = bincode::serialize(&executed)
                    .map_err(Error::from)
                    .map_err(bonsaidb_core::Error::from)
                    .map_err(ConflictableTransactionError::Abort)?;
                tree.insert(&executed.id.to_be_bytes(), serialized)?;

                trees.iter().for_each(TransactionalTree::flush);
                Ok(results)
            }) {
                Ok(results) => Ok(results),
                Err(err) => match err {
                    TransactionError::Abort(err) => Err(err),
                    TransactionError::Storage(err) => {
                        Err(bonsaidb_core::Error::Database(err.to_string()))
                    }
                },
            }
        })
        .await
        .map_err(|err| bonsaidb_core::Error::Database(err.to_string()))?
    }

    async fn get<C: schema::Collection>(
        &self,
        id: u64,
    ) -> Result<Option<Document<'static>>, bonsaidb_core::Error> {
        self.get_from_collection_id(id, &C::collection_name()?)
            .await
    }

    async fn get_multiple<C: schema::Collection>(
        &self,
        ids: &[u64],
    ) -> Result<Vec<Document<'static>>, bonsaidb_core::Error> {
        self.get_multiple_from_collection_id(ids, &C::collection_name()?)
            .await
    }

    async fn list_executed_transactions(
        &self,
        starting_id: Option<u64>,
        result_limit: Option<usize>,
    ) -> Result<Vec<transaction::Executed<'static>>, bonsaidb_core::Error> {
        let result_limit = result_limit
            .unwrap_or(LIST_TRANSACTIONS_DEFAULT_RESULT_COUNT)
            .min(LIST_TRANSACTIONS_MAX_RESULTS);
        if result_limit > 0 {
            let task_self = self.clone();
            let transaction_tree_name = transaction_tree_name(&self.data.name);
            tokio::task::spawn_blocking(move || {
                let tree = task_self
                    .sled()
                    .open_tree(&transaction_tree_name)
                    .map_err(Error::from)?;
                let iter = if let Some(starting_id) = starting_id {
                    tree.range(starting_id.to_be_bytes()..=u64::MAX.to_be_bytes())
                } else {
                    tree.iter()
                };

                #[allow(clippy::cast_possible_truncation)] // this value is limited above
                let mut results = Vec::with_capacity(result_limit);
                for row in iter {
                    let (_, vec) = row.map_err(Error::from)?;
                    results.push(
                        bincode::deserialize::<transaction::Executed<'_>>(&vec)
                            .map_err(Error::from)?
                            .to_owned(),
                    );

                    if results.len() >= result_limit {
                        break;
                    }
                }
                Ok(results)
            })
            .await
            .unwrap()
        } else {
            // A request was made to return an empty result? This should probably be
            // an error, but technically this is a correct response.
            Ok(Vec::default())
        }
    }

    #[must_use]
    async fn query<V: schema::View>(
        &self,
        key: Option<QueryKey<V::Key>>,
        access_policy: AccessPolicy,
    ) -> Result<Vec<Map<V::Key, V::Value>>, bonsaidb_core::Error>
    where
        Self: Sized,
    {
        let mut results = Vec::new();
        self.for_each_view_entry::<V, _>(key, access_policy, |collection| {
            let entry = ViewEntry::from(collection);
            let key = <V::Key as Key>::from_big_endian_bytes(&entry.key)
                .map_err(view::Error::KeySerialization)
                .map_err(Error::from)?;
            for entry in entry.mappings {
                results.push(Map {
                    source: entry.source,
                    key: key.clone(),
                    value: serde_cbor::from_slice(&entry.value).map_err(Error::Serialization)?,
                });
            }
            Ok(())
        })
        .await?;

        Ok(results)
    }

    async fn query_with_docs<V: schema::View>(
        &self,
        key: Option<QueryKey<V::Key>>,
        access_policy: AccessPolicy,
    ) -> Result<Vec<MappedDocument<V::Key, V::Value>>, bonsaidb_core::Error>
    where
        Self: Sized,
    {
        let results = Connection::query::<V>(self, key, access_policy).await?;

        let mut documents = self
            .get_multiple::<V::Collection>(&results.iter().map(|m| m.source).collect::<Vec<_>>())
            .await?
            .into_iter()
            .map(|doc| (doc.header.id, doc))
            .collect::<HashMap<_, _>>();

        Ok(results
            .into_iter()
            .filter_map(|map| {
                if let Some(document) = documents.remove(&map.source) {
                    Some(MappedDocument {
                        key: map.key,
                        value: map.value,
                        document,
                    })
                } else {
                    None
                }
            })
            .collect())
    }

    async fn reduce<V: schema::View>(
        &self,
        key: Option<QueryKey<V::Key>>,
        access_policy: AccessPolicy,
    ) -> Result<V::Value, bonsaidb_core::Error>
    where
        Self: Sized,
    {
        let view = self
            .data
            .schema
            .view::<V>()
            .expect("query made with view that isn't registered with this database");

        let result = self
            .reduce_in_view(
                &view.view_name()?,
                key.map(|key| key.serialized()).transpose()?,
                access_policy,
            )
            .await?;
        let value = serde_cbor::from_slice(&result).map_err(Error::Serialization)?;

        Ok(value)
    }

    async fn reduce_grouped<V: schema::View>(
        &self,
        key: Option<QueryKey<V::Key>>,
        access_policy: AccessPolicy,
    ) -> Result<Vec<MappedValue<V::Key, V::Value>>, bonsaidb_core::Error>
    where
        Self: Sized,
    {
        let view = self
            .data
            .schema
            .view::<V>()
            .expect("query made with view that isn't registered with this database");

        let results = self
            .grouped_reduce_in_view(
                &view.view_name()?,
                key.map(|key| key.serialized()).transpose()?,
                access_policy,
            )
            .await?;
        results
            .into_iter()
            .map(|map| {
                Ok(MappedValue {
                    key: V::Key::from_big_endian_bytes(&map.key)
                        .map_err(view::Error::KeySerialization)?,
                    value: serde_cbor::from_slice(&map.value)?,
                })
            })
            .collect::<Result<Vec<_>, bonsaidb_core::Error>>()
    }

    async fn last_transaction_id(&self) -> Result<Option<u64>, bonsaidb_core::Error> {
        let task_self = self.clone();
        let transaction_tree_name = transaction_tree_name(&self.data.name);
        tokio::task::spawn_blocking(move || {
            let tree = task_self
                .sled()
                .open_tree(&transaction_tree_name)
                .map_err(Error::from)?;
            if let Some((key, _)) = tree.last().map_err(Error::from)? {
                Ok(Some(
                    u64::from_big_endian_bytes(&key).map_err(view::Error::KeySerialization)?,
                ))
            } else {
                Ok(None)
            }
        })
        .await
        .unwrap()
    }
}

type ViewIterator<'a> = Box<dyn Iterator<Item = Result<(IVec, IVec), sled::Error>> + 'a>;
type ViewEntryIterator<'a> =
    Box<dyn Iterator<Item = Result<ViewEntryCollection, crate::Error>> + 'a>;

struct ViewEntryCollectionIterator<'a> {
    iterator: ViewIterator<'a>,
    permissions: Option<&'a Permissions>,
    vault: &'a Vault,
}

impl<'a> Iterator for ViewEntryCollectionIterator<'a> {
    type Item = Result<ViewEntryCollection, crate::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.iterator.next().map(|item| {
            item.map_err(crate::Error::from)
                .and_then(|(_, value)| self.vault.decrypt_serialized(self.permissions, &value))
        })
    }
}

struct KeyViewEntryIterator<'a> {
    tree: &'a Tree,
    keys: std::vec::IntoIter<Vec<u8>>,
    view: &'a dyn view::Serialized,
    encrypt_by_default: bool,
    permissions: Option<&'a Permissions>,
    vault: &'a Vault,
}

impl<'a> Iterator for KeyViewEntryIterator<'a> {
    type Item = Result<ViewEntryCollection, crate::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(key) = self.keys.next() {
            match mapper::load_entry_for_key(
                &key,
                self.view,
                self.encrypt_by_default,
                self.vault,
                self.permissions,
                |key| {
                    self.tree
                        .get(&key)
                        .map_err(ConflictableTransactionError::Storage)
                },
            ) {
                Ok(Some(entry)) => return Some(Ok(entry)),
                Ok(None) => {}
                Err(err) => {
                    return Some(Err(match err {
                        ConflictableTransactionError::Abort(core) => crate::Error::Core(core),
                        ConflictableTransactionError::Storage(sled) => crate::Error::Sled(sled),
                        ConflictableTransactionError::Conflict => unreachable!(),
                    }))
                }
            }
        }
        None
    }
}

pub fn transaction_tree_name(database: &str) -> String {
    format!("{}::transactions", database)
}

pub fn document_tree_name(database: &str, collection: &CollectionName) -> String {
    format!("{}::collection::{}", database, collection)
}

#[async_trait]
impl<DB> OpenDatabase for Database<DB>
where
    DB: Schema,
{
    fn as_any(&self) -> &'_ dyn Any {
        self
    }

    async fn get_from_collection_id(
        &self,
        id: u64,
        collection: &CollectionName,
        permissions: &Permissions,
    ) -> Result<Option<Document<'static>>, bonsaidb_core::Error> {
        self.with_effective_permissions(permissions.clone())
            .get_from_collection_id(id, collection)
            .await
    }

    async fn get_multiple_from_collection_id(
        &self,
        ids: &[u64],
        collection: &CollectionName,
        permissions: &Permissions,
    ) -> Result<Vec<Document<'static>>, bonsaidb_core::Error> {
        self.with_effective_permissions(permissions.clone())
            .get_multiple_from_collection_id(ids, collection)
            .await
    }

    async fn apply_transaction(
        &self,
        transaction: Transaction<'static>,
        permissions: &Permissions,
    ) -> Result<Vec<OperationResult>, bonsaidb_core::Error> {
        <Self as Connection>::apply_transaction(
            &self.with_effective_permissions(permissions.clone()),
            transaction,
        )
        .await
    }

    async fn query(
        &self,
        view: &ViewName,
        key: Option<QueryKey<Vec<u8>>>,
        access_policy: AccessPolicy,
    ) -> Result<Vec<map::Serialized>, bonsaidb_core::Error> {
        if let Some(view) = self.schematic().view_by_name(view) {
            let mut results = Vec::new();
            self.for_each_in_view(view, key, access_policy, |collection| {
                let entry = ViewEntry::from(collection);
                for mapping in entry.mappings {
                    results.push(map::Serialized {
                        source: mapping.source,
                        key: entry.key.clone(),
                        value: mapping.value,
                    });
                }
                Ok(())
            })
            .await?;

            Ok(results)
        } else {
            Err(bonsaidb_core::Error::CollectionNotFound)
        }
    }

    async fn query_with_docs(
        &self,
        view: &ViewName,
        key: Option<QueryKey<Vec<u8>>>,
        access_policy: AccessPolicy,
        permissions: &Permissions,
    ) -> Result<Vec<networking::MappedDocument>, bonsaidb_core::Error> {
        let results = OpenDatabase::query(self, view, key, access_policy).await?;
        let view = self.schematic().view_by_name(view).unwrap(); // query() will fail if it's not present

        let mut documents = self
            .with_effective_permissions(permissions.clone())
            .get_multiple_from_collection_id(
                &results.iter().map(|m| m.source).collect::<Vec<_>>(),
                &view.collection()?,
            )
            .await?
            .into_iter()
            .map(|doc| (doc.header.id, doc))
            .collect::<HashMap<_, _>>();

        Ok(results
            .into_iter()
            .filter_map(|map| {
                if let Some(source) = documents.remove(&map.source) {
                    Some(networking::MappedDocument {
                        key: map.key,
                        value: map.value,
                        source,
                    })
                } else {
                    None
                }
            })
            .collect())
    }

    async fn reduce(
        &self,
        view: &ViewName,
        key: Option<QueryKey<Vec<u8>>>,
        access_policy: AccessPolicy,
    ) -> Result<Vec<u8>, bonsaidb_core::Error> {
        self.reduce_in_view(view, key, access_policy).await
    }

    async fn reduce_grouped(
        &self,
        view: &ViewName,
        key: Option<QueryKey<Vec<u8>>>,
        access_policy: AccessPolicy,
    ) -> Result<Vec<MappedValue<Vec<u8>, Vec<u8>>>, bonsaidb_core::Error> {
        self.grouped_reduce_in_view(view, key, access_policy).await
    }

    async fn list_executed_transactions(
        &self,
        starting_id: Option<u64>,
        result_limit: Option<usize>,
    ) -> Result<Vec<Executed<'static>>, bonsaidb_core::Error> {
        Connection::list_executed_transactions(self, starting_id, result_limit).await
    }

    async fn last_transaction_id(&self) -> Result<Option<u64>, bonsaidb_core::Error> {
        Connection::last_transaction_id(self).await
    }

    #[cfg(feature = "keyvalue")]
    async fn execute_key_operation(
        &self,
        op: KeyOperation,
    ) -> Result<Output, bonsaidb_core::Error> {
        Kv::execute_key_operation(self, op).await
    }
}