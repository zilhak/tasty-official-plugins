//! Codex 호출자가 전달한 실행 환경. 인증 정보나 다른 환경 변수는 보관하지 않는다.

use std::path::PathBuf;
use std::process::Command;

use serde::{Deserialize, Serialize};
use tasty_utils::execution::ExecutionContext;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CodexExecution {
    pub executable: PathBuf,
    pub search_path: String,
}

impl CodexExecution {
    pub fn validate(&self) -> std::io::Result<ExecutionContext> {
        ExecutionContext::from_parts(self.executable.clone(), self.search_path.clone().into())
    }

    pub fn command(&self) -> std::io::Result<Command> {
        Ok(self.validate()?.command())
    }
}
