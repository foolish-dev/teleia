//! Chain multiple [`ToolRouter`]s under a single agent. Telia uses one
//! for MCP tools and one for LSP tools, but [`telia_agent::Agent`]
//! only accepts a single router — this collapses N routers into one
//! by name-routing through each child's `handles()` predicate.

use anyhow::{anyhow, Result};
use futures_util::future::BoxFuture;
use telia_agent::ToolRouter;
use telia_llm::ToolDef;

pub struct CombinedRouter {
    routers: Vec<Box<dyn ToolRouter>>,
}

impl CombinedRouter {
    pub fn new(routers: Vec<Box<dyn ToolRouter>>) -> Self {
        Self { routers }
    }
}

impl ToolRouter for CombinedRouter {
    fn definitions(&self) -> Vec<ToolDef> {
        self.routers.iter().flat_map(|r| r.definitions()).collect()
    }
    fn handles(&self, name: &str) -> bool {
        self.routers.iter().any(|r| r.handles(name))
    }
    fn dispatch<'a>(&'a mut self, name: &'a str, args: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            for r in self.routers.iter_mut() {
                if r.handles(name) {
                    return r.dispatch(name, args).await;
                }
            }
            Err(anyhow!("no router handles `{name}`"))
        })
    }
}
