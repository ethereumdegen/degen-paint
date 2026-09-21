//! The project overview, in one place.
//!
//! Three surfaces answer "what is in this project": MCP's `dpaint_overview`, the Studio's
//! `overview` dispatch and `GET /api/v1/overview`. They must be the same bytes — a grounding
//! probe that disagrees with the tool the agent just called is worse than no probe.

use crate::journal::Entry;
use crate::{AssetStore, Document, Project, Result};
use serde_json::{json, Value};

/// Documents, their contents in counts, the asset situation and the tail of the journal.
/// `only` narrows `documents` to a single document id or name.
pub fn overview(
    project: &Project,
    entries: &[Entry],
    assets: &AssetStore,
    only: Option<&str>,
) -> Result<Value> {
    let recent: Vec<Value> = entries
        .iter()
        .rev()
        .take(5)
        .map(
            |e| json!({ "seq": e.seq, "op": e.op, "at": e.ts, "actor": e.actor, "undone": e.undone }),
        )
        .collect();
    let documents: Vec<Value> = project
        .documents
        .values()
        .filter(|d| match only {
            Some(want) => d.id().as_str() == want || d.name() == want,
            None => true,
        })
        .map(describe_document)
        .collect();

    Ok(json!({
        "project": {
            "id": project.id,
            "name": project.name,
            "active": project.active,
            "modified": project.modified,
        },
        "documents": documents,
        "assets": {
            "referenced": project.referenced_assets().len(),
            "onDisk": assets.list().map(|a| a.len()).unwrap_or(0),
        },
        "history": { "entries": entries.len(), "recent": recent },
    }))
}

fn describe_document(doc: &Document) -> Value {
    let mut v = json!({
        "id": doc.id(),
        "name": doc.name(),
        "kind": doc.kind().as_str(),
    });
    if let Some((w, h)) = doc.size() {
        v["size"] = json!([w, h]);
    }
    match doc {
        Document::Raster(d) => {
            v["layers"] = json!(d.walk().len());
            v["hasSelection"] = json!(d.selection.is_some());
        }
        Document::Vector(d) => {
            v["objects"] = json!(d.walk().len());
            v["artboards"] = json!(d.artboards.len());
        }
        Document::Model(d) => {
            v["nodes"] = json!(d.nodes.len());
            v["meshes"] = json!(d.meshes.len());
            v["materials"] = json!(d.materials.len());
        }
    }
    v
}
