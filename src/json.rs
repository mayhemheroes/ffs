use std::collections::HashMap;

use serde_json::Value;

use tracing::{info, instrument};

use fuser::FileType;

use super::config::Config;
use super::fs::{DirEntry, DirType, Entry, Inode, FS};

/// Parses JSON into a value; just a shim for `serde_json::from_reader`.
#[instrument(level = "info", skip(reader))]
pub fn parse(reader: Box<dyn std::io::BufRead>) -> Value {
    serde_json::from_reader(reader).expect("JSON")
}

/// Predicts filetypes from JSON values.
///
/// `Value::Object` and `Value::Array` map to directories; everything else is a
/// regular file.
fn kind(v: &Value) -> FileType {
    match v {
        Value::Object(_) | Value::Array(_) => FileType::Directory,
        _ => FileType::RegularFile,
    }
}

/// Calculates the size of a JSON value, i.e., the number of AST nodes used to
/// represent it. Used for pre-allocating space for inodes in `fs()` below.
fn size(v: &Value) -> usize {
    match v {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => 1,
        Value::Array(vs) => vs.iter().map(|v| size(v)).sum::<usize>() + 1,
        Value::Object(fvs) => fvs.iter().map(|(_, v)| size(v)).sum::<usize>() + 1,
    }
}

/// Generates `fs::FS` from a `serde_json::Value` in a particular `Config`.
///
/// The current implementation is eager: it preallocates enough inodes and then
/// fills them in using a depth-first traversal.
///
/// Invariant: the index in the vector is the inode number. Inode 0 is invalid,
/// and is left empty.
#[instrument(level = "info", skip(v, config))]
pub fn fs(config: Config, v: Value) -> FS {
    let mut inodes: Vec<Option<Inode>> = Vec::new();

    // reserve space for everyone else
    // won't work with streaming or lazy generation, but avoids having to resize the vector midway through
    inodes.resize_with(size(&v) + 1, || None);
    info!("allocated {} inodes", inodes.len());

    let mut next_id = fuser::FUSE_ROOT_ID;
    // parent inum, inum, value
    let mut worklist: Vec<(u64, u64, Value)> = Vec::new();

    if !(v.is_array() || v.is_object()) {
        panic!(
            "Unable to build a filesystem out of the primitive value '{}'",
            v
        );
    }
    worklist.push((next_id, next_id, v));
    next_id += 1;

    while !worklist.is_empty() {
        let (parent, inum, v) = worklist.pop().unwrap();

        let nl = if config.add_newlines { "\n" } else { "" };
        let entry = match v {
            Value::Null => Entry::File(nl.into()),
            Value::Bool(b) => Entry::File(format!("{}{}", b, nl).into_bytes()),
            Value::Number(n) => Entry::File(format!("{}{}", n, nl).into_bytes()),
            Value::String(s) => {
                let contents = if s.ends_with('\n') { s } else { s + nl };
                Entry::File(contents.into_bytes())
            },
            Value::Array(vs) => {
                let mut children = HashMap::new();
                children.reserve(vs.len());

                let num_elts = vs.len() as f64;
                let width = num_elts.log10().ceil() as usize;

                for (i, child) in vs.into_iter().enumerate() {
                    // TODO 2021-06-08 ability to add prefixes
                    let name = if config.pad_element_names {
                        format!("{:0width$}", i, width = width)
                    } else {
                        format!("{}", i)
                    };

                    children.insert(
                        name,
                        DirEntry {
                            inum: next_id,
                            kind: kind(&child),
                        },
                    );
                    worklist.push((inum, next_id, child));
                    next_id += 1;
                }

                Entry::Directory(DirType::List, children)
            }
            Value::Object(fvs) => {
                let mut children = HashMap::new();
                children.reserve(fvs.len());

                for (field, child) in fvs.into_iter() {
                    let original = field.clone();
                    let mut nfield = config.normalize_name(field);

                    while children.contains_key(&nfield) {
                        nfield.push('_');
                    }

                    if original != nfield {
                        info!(
                            "renamed {} to {} (inode {} with parent {})",
                            original, nfield, next_id, parent
                        );
                    }

                    children.insert(
                        nfield,
                        DirEntry {
                            inum: next_id,
                            kind: kind(&child),
                        },
                    );

                    worklist.push((inum, next_id, child));
                    next_id += 1;
                }

                Entry::Directory(DirType::Named, children)
            }
        };

        inodes[inum as usize] = Some(Inode {
            parent,
            inum,
            entry,
        });
    }
    assert_eq!(inodes.len() as u64, next_id);

    FS { inodes, config }
}
