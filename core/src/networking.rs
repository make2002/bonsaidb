use std::borrow::Cow;

use async_trait::async_trait;
use circulate::Message;
pub use fabruic;
use schema::SchemaId;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::{
    connection::{AccessPolicy, QueryKey},
    document::Document,
    schema::{
        self,
        view::{self, map},
        CollectionId, Key, MappedValue,
    },
    transaction::{Executed, OperationResult, Transaction},
};

/// A payload with an associated id.
#[derive(Clone, Deserialize, Serialize, Debug)]
pub struct Payload<T> {
    /// The unique id for this payload.
    pub id: Option<u32>,
    /// The wrapped payload.
    pub wrapped: T,
}

/// A request made to a server.
#[derive(Clone, Deserialize, Serialize, Debug)]
pub enum Request {
    /// A server-related request.
    Server(ServerRequest),
    /// A database-related request.
    Database {
        /// The name of the database.
        database: String,
        /// The request made to the database.
        request: DatabaseRequest,
    },
}

/// A server-related request.
#[derive(Clone, Deserialize, Serialize, Debug)]
pub enum ServerRequest {
    /// Creates a database.
    CreateDatabase(Database),
    /// Deletes the database named `name`
    DeleteDatabase {
        /// The name of the database to delete.
        name: String,
    },
    /// Lists all databases.
    ListDatabases,
    /// Lists available schemas.
    ListAvailableSchemas,
}

/// A database-related request.
#[derive(Clone, Deserialize, Serialize, Debug)]
pub enum DatabaseRequest {
    /// Retrieve a single document.
    Get {
        /// The collection of the document.
        collection: CollectionId,
        /// The id of the document.
        id: u64,
    },
    /// Retrieve multiple documents.
    GetMultiple {
        /// The collection of the documents.
        collection: CollectionId,
        /// The ids of the documents.
        ids: Vec<u64>,
    },
    /// Queries a view.
    Query {
        /// The name of the view.
        view: Cow<'static, str>,
        /// The filter for the view.
        key: Option<QueryKey<Vec<u8>>>,
        /// The access policy for the query.
        access_policy: AccessPolicy,
        /// If true, [`DatabaseResponse::ViewMappingsWithDocs`] will be
        /// returned. If false, [`DatabaseResponse::ViewMappings`] will be
        /// returned.
        with_docs: bool,
    },
    /// Reduces a view.
    Reduce {
        /// The name of the view.
        view: Cow<'static, str>,
        /// The filter for the view.
        key: Option<QueryKey<Vec<u8>>>,
        /// The access policy for the query.
        access_policy: AccessPolicy,
        /// Whether to return a single value or values grouped by unique key. If
        /// true, [`DatabaseResponse::ViewGroupedReduction`] will be returned.
        /// If false, [`DatabaseResponse::ViewReduction`] is returned.
        grouped: bool,
    },
    /// Applies a transaction.
    ApplyTransaction {
        /// The trasnaction to apply.
        transaction: Transaction<'static>,
    },
    /// Lists executed transactions.
    ListExecutedTransactions {
        /// The starting transaction id.
        starting_id: Option<u64>,
        /// The maximum number of results.
        result_limit: Option<usize>,
    },
    /// Queries the last transaction id.
    LastTransactionId,
    /// Creates a `PubSub` [`Subscriber`](crate::pubsub::Subscriber)
    CreateSubscriber,
    /// Publishes a `PubSub` message.
    Publish(Message),
    /// Subscribes `subscriber_id` to messages for `topic`.
    SubscribeTo {
        /// The id of the [`Subscriber`](crate::pubsub::Subscriber).
        subscriber_id: u64,
        /// The topic to subscribe to.
        topic: String,
    },
    /// Unsubscribes `subscriber_id` from messages for `topic`.
    UnsubscribeFrom {
        /// The id of the [`Subscriber`](crate::pubsub::Subscriber).
        subscriber_id: u64,
        /// The topic to unsubscribe from.
        topic: String,
    },
    /// Unregisters the subscriber.
    UnregisterSubscriber {
        /// The id of the [`Subscriber`](crate::pubsub::Subscriber).
        subscriber_id: u64,
    },
}

/// A response from a server.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum Response {
    /// A request succeded but provided no output.
    Ok,
    /// A response to a [`ServerRequest`].
    Server(ServerResponse),
    /// A response to a [`DatabaseRequest`].
    Database(DatabaseResponse),
    /// An error occurred processing a request.
    Error(crate::Error),
}

/// A response to a [`ServerRequest`].
#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum ServerResponse {
    /// A database with `name` was successfully created.
    DatabaseCreated {
        /// The name of the database to create.
        name: String,
    },
    /// A database with `name` was successfully removed.
    DatabaseDeleted {
        /// The name of the database to remove.
        name: String,
    },
    /// A list of available databases.
    Databases(Vec<Database>),
    ///A list of availble schemas.
    AvailableSchemas(Vec<SchemaId>),
}

/// A response to a [`DatabaseRequest`].
#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum DatabaseResponse {
    /// One or more documents.
    Documents(Vec<Document<'static>>),
    /// Results of [`DatabaseRequest::ApplyTransaction`].
    TransactionResults(Vec<OperationResult>),
    /// Results of [`DatabaseRequest::Query`] when `with_docs` is false.
    ViewMappings(Vec<map::Serialized>),
    /// Results of [`DatabaseRequest::Query`] when `with_docs` is true.
    ViewMappingsWithDocs(Vec<MappedDocument>),
    /// Result of [`DatabaseRequest::Reduce`] when `grouped` is false.
    ViewReduction(Vec<u8>),
    /// Result of [`DatabaseRequest::Reduce`] when `grouped` is true.
    ViewGroupedReduction(Vec<MappedValue<Vec<u8>, Vec<u8>>>),
    /// Results of [`DatabaseRequest::ListExecutedTransactions`].
    ExecutedTransactions(Vec<Executed<'static>>),
    /// Result of [`DatabaseRequest::LastTransactionId`].
    LastTransactionId(Option<u64>),
    /// A new `PubSub` subscriber was created.
    SubscriberCreated {
        /// The unique ID of the subscriber.
        subscriber_id: u64,
    },
    /// A PubSub message was received.
    MessageReceived {
        /// The ID of the subscriber receiving the message.
        subscriber_id: u64,
        /// The message received.
        message: Message,
    },
}

/// A serialized [`MappedDocument`](map::MappedDocument).
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct MappedDocument {
    /// The serialized key.
    pub key: Vec<u8>,
    /// The serialized value.
    pub value: Vec<u8>,
    /// The source document.
    pub source: Document<'static>,
}

impl MappedDocument {
    /// Deserialize into a [`MappedDocument`](map::MappedDocument).
    pub fn deserialized<K: Key, V: Serialize + DeserializeOwned>(
        self,
    ) -> Result<map::MappedDocument<K, V>, crate::Error> {
        let key = Key::from_big_endian_bytes(&self.key)
            .map_err(|err| crate::Error::Storage(view::Error::KeySerialization(err).to_string()))?;
        let value = serde_cbor::from_slice(&self.value)
            .map_err(|err| crate::Error::Storage(view::Error::from(err).to_string()))?;

        Ok(map::MappedDocument {
            document: self.source,
            key,
            value,
        })
    }
}

/// A database on a server.
#[derive(Clone, PartialEq, Deserialize, Serialize, Debug)]
pub struct Database {
    /// The name of the database.
    pub name: String,
    /// The schema defining the database.
    pub schema: SchemaId,
}

/// Functions for interacting with a `PliantDB` server.
#[async_trait]
pub trait ServerConnection: Send + Sync {
    /// Creates a database named `name` using the [`SchemaId`] `schema`.
    ///
    /// ## Errors
    ///
    /// * [`Error::InvalidDatabaseName`]: `name` must begin with an alphanumeric
    ///   character (`[a-zA-Z0-9]`), and all remaining characters must be
    ///   alphanumeric, a period (`.`), or a hyphen (`-`).
    /// * [`Error::DatabaseNameAlreadyTaken]: `name` was already used for a
    ///   previous database name. Database names are case insensitive.
    async fn create_database(&self, name: &str, schema: SchemaId) -> Result<(), crate::Error>;

    /// Deletes a database named `name`.
    ///
    /// ## Errors
    ///
    /// * [`Error::DatabaseNotFound`]: database `name` does not exist.
    /// * [`Error::Core(core::Error::Io)`]: an error occurred while deleting files.
    async fn delete_database(&self, name: &str) -> Result<(), crate::Error>;

    /// Lists the databases on this server.
    async fn list_databases(&self) -> Result<Vec<Database>, crate::Error>;

    /// Lists the [`SchemaId`]s on this server.
    async fn list_available_schemas(&self) -> Result<Vec<SchemaId>, crate::Error>;
}

/// A networking error.
#[derive(Clone, thiserror::Error, Debug, Serialize, Deserialize)]
pub enum Error {
    /// An invalid database name was specified. See
    /// [`ServerConnection::create_database()`] for database name requirements.
    #[error("invalid database name: {0}")]
    InvalidDatabaseName(String),

    /// The database name given was not found.
    #[error("database '{0}' was not found")]
    DatabaseNotFound(String),

    /// The database name already exists.
    #[error("a database with name '{0}' already exists")]
    DatabaseNameAlreadyTaken(String),

    /// The server responded with a message that wasn't expected for the request
    /// sent.
    #[error("unexpected response: {0}")]
    UnexpectedResponse(String),

    /// The connection was interrupted.
    #[error("unexpected disconnection")]
    Disconnected,

    /// The database named `database_name` was created with a different schema
    /// (`stored_schema`) than provided (`schema`).
    #[error(
        "database '{database_name}' was created with schema '{stored_schema}', not '{schema}'"
    )]
    SchemaMismatch {
        /// The name of the database being accessed.
        database_name: String,

        /// The schema provided for the database.
        schema: SchemaId,

        /// The schema stored for the database.
        stored_schema: SchemaId,
    },

    /// The [`SchemaId`] returned has already been registered with this server.
    #[error("schema '{0}' was already registered")]
    SchemaAlreadyRegistered(SchemaId),

    /// The [`SchemaId`] requested was not registered with this server.
    #[error("schema '{0}' is not registered with this server")]
    SchemaNotRegistered(SchemaId),
}