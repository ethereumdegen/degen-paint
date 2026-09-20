//! degen-paint's 3D mode: procedural meshes, PBR materials, a glTF-shaped scene graph,
//! keyframe animation, and glTF 2.0 export.
//!
//! Geometry is a *recipe*, not a vertex soup. A model document says "extrude `logo:#mark`
//! 12 units deep with a 1.5 bevel"; [`build_mesh`] turns that into triangles on demand, so
//! editing the source path in the vector document regenerates the solid. Baking verbs
//! (`model.mesh.weld`, `.decimate`, `.transform-bake`, `.merge`, `.import`) are explicit and
//! write their result to the content-addressed asset store.

pub mod build;
pub mod export;
pub mod extrude;
pub mod geom;
pub mod ops;
pub mod prim;
pub mod uv;
pub mod validate;

pub use build::{
    build_mesh, decode_blob, encode_blob, mesh_tangents, scene_meshes, world_transforms,
};
pub use export::{export, GltfOut, TextureResolver};
pub use geom::MeshData;
pub use ops::ops;
pub use validate::{Finding, Severity};
