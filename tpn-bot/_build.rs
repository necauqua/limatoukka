// use std::path::PathBuf;

// use schemars::schema::{InstanceType, Metadata, SchemaObject};

// include!("src/config/def.rs");

// fn main() -> anyhow::Result<()> {
//     let out = std::env::var("CARGO_MANIFEST_DIR")?;
//     let mut schema = schemars::schema_for!(Config);

//     schema.schema.object.as_mut().unwrap().properties.insert(
//         "$schema".into(),
//         SchemaObject {
//             metadata: Some(
//                 Metadata {
//                     description: Some("The JSON Schema of this document".into()),
//                     ..Default::default()
//                 }
//                 .into(),
//             ),
//             instance_type: Some(InstanceType::String.into()),
//             ..Default::default()
//         }
//         .into(),
//     );
//     schema
//         .schema
//         .extensions
//         .insert("additionalProperties".into(), false.into());

//     std::fs::write(
//         PathBuf::from(out).join("target").join("config-schema.json"),
//         serde_json::to_string_pretty(&schema)?,
//     )?;
//     Ok(())
// }

fn main() {}
