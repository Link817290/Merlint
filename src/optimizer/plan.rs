use serde::Serialize;

/// A complete optimization plan generated from session analysis
#[derive(Debug, Serialize)]
pub struct OptimizationPlan {
    pub items: Vec<OptimizationItem>,
    pub estimated_token_savings_per_call: i64,
    pub estimated_cache_improvement_pct: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct OptimizationItem {
    pub id: String,
    pub category: OptCategory,
    pub severity: OptSeverity,
    pub title: String,
    pub description: String,
    pub estimated_savings: i64,
    pub action: OptAction,
}

#[derive(Debug, Clone, Serialize)]
pub enum OptCategory {
    Tools,
    Cache,
    Prompt,
    Efficiency,
}

#[derive(Debug, Clone, Serialize)]
pub enum OptSeverity {
    /// Large savings, should definitely fix
    High,
    /// Moderate savings
    Medium,
    /// Minor improvement
    Low,
}

#[derive(Debug, Clone, Serialize)]
pub enum OptAction {
    /// Remove unused tools — contains the tool names to remove
    PruneTools { remove: Vec<String>, keep: Vec<String> },
    /// Restructure prompt for better cache hits
    RestructurePrompt { suggestion: String },
    /// Generate/update CLAUDE.md or agent config
    GenerateConfig { file_path: String, content: String },
    /// Add caching hints to avoid redundant reads
    AddCacheHints { resources: Vec<String>, hint: String },
    /// Reduce retry patterns
    ReduceRetries { pattern: String, suggestion: String },
}

impl OptimizationPlan {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            estimated_token_savings_per_call: 0,
            estimated_cache_improvement_pct: 0.0,
        }
    }

    pub fn add(&mut self, item: OptimizationItem) {
        self.estimated_token_savings_per_call += item.estimated_savings;
        self.items.push(item);
    }

    pub fn sort_by_impact(&mut self) {
        self.items.sort_by(|a, b| b.estimated_savings.cmp(&a.estimated_savings));
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn total_savings(&self) -> i64 {
        self.items.iter().map(|i| i.estimated_savings).sum()
    }
}
