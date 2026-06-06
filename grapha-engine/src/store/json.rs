use std::path::PathBuf;

use grapha_core::graph::Graph;

use super::Store;

pub struct JsonStore {
    path: PathBuf,
}

impl JsonStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Store for JsonStore {
    fn save(&self, graph: &Graph) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(graph)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, json)?;
        Ok(())
    }

    fn load(&self) -> anyhow::Result<Graph> {
        let content = std::fs::read_to_string(&self.path)?;
        let graph = serde_json::from_str(&content)?;
        Ok(graph)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_store_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.json");
        let store = JsonStore::new(path);

        let graph = Graph::new();
        store.save(&graph).unwrap();
        assert!(store.path.exists());

        let loaded = store.load().unwrap();
        assert_eq!(loaded.version, graph.version);
    }
}
