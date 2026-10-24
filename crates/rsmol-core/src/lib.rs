//! rsmol-core: molecular data model, file I/O and (soon) the selection engine.
//!
//! This crate has no UI or GPU dependencies so it can be used from the desktop app, WASM and
//! the Python bindings alike.

mod element;
pub mod io;
pub mod select;
mod structure;

pub use element::Element;
pub use structure::{
    Atoms, Bond, BondKind, Chain, Entity, EntityKind, Residue, SecondaryStructure, Structure, Vec3,
};
