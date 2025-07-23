use std::{path::PathBuf, time::SystemTime};

use anyhow::Context;
use clap::Args;

#[derive(Debug, Args)]
#[clap(about = "Get the last write time in secs for the path", hide = true)]
pub struct LastModified {
    pub path: PathBuf,
}

impl LastModified {
    pub fn run(self) -> anyhow::Result<()> {
        let metadata = self.path.metadata()?;
        let last_modified = metadata
            .modified()?
            .duration_since(SystemTime::UNIX_EPOCH)
            .context("Clock may have gone backwards")?;
        println!("{}", last_modified.as_secs());
        Ok(())
    }
}
