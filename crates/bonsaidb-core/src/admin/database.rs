use serde::{Deserialize, Serialize};

use crate::{
    define_basic_unique_mapped_view,
    schema::{
        Collection, CollectionDocument, CollectionName, InvalidNameError, SchemaName, Schematic,
    },
    Error,
};

/// A database stored in `BonsaiDb`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Database {
    /// The name of the database.
    pub name: String,
    /// The schema defining the database.
    pub schema: SchemaName,
}

impl Collection for Database {
    fn collection_name() -> Result<CollectionName, InvalidNameError> {
        CollectionName::new("bonsaidb", "databases")
    }

    fn define_views(schema: &mut Schematic) -> Result<(), Error> {
        schema.define_view(ByName)
    }
}

define_basic_unique_mapped_view!(
    ByName,
    Database,
    1,
    "by-name",
    String,
    SchemaName,
    |document: CollectionDocument<Database>| {
        vec![document.header.emit_key_and_value(
            document.contents.name.to_ascii_lowercase(),
            document.contents.schema,
        )]
    },
);