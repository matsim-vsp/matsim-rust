use super::*;

/// Publish the diagnostics of a failed attempt. The completed report in [`ANALYSIS_DIR`] is never
/// touched, so a failed rerun cannot be mistaken for a completed index.
pub(super) fn publish_failure(
    output_dir: &Path,
    manifest: &Manifest,
    error: &AnalysisError,
) -> Result<(), AnalysisError> {
    let mut failed = manifest.clone();
    failed.status = STATUS_FAILED.to_owned();
    failed.failure = Some(error.to_string());
    let statuses = module_statuses(
        &RequiredOutcome::Failed(error.to_string()),
        None,
        None,
        ZoneTables::NotPublished,
        None,
        None,
        None,
        &TransitOutcome::default(),
        None,
        None,
        None,
    );
    let staging = output_dir.join(FAILURE_STAGING_DIR);
    reset_staging(&staging)?;
    write_json(&staging.join(MANIFEST_FILE), &failed)?;
    write_json(&staging.join(MODULE_STATUS_FILE), &statuses)?;
    fs::write(staging.join("failure.txt"), format!("{error}\n")).map_err(io_error)?;
    report::write_failure_report(&staging, &failed, &statuses)?;
    publish(
        &staging,
        &output_dir.join(FAILURE_DIR),
        &output_dir.join(FAILURE_BACKUP_DIR),
    )?;
    Ok(())
}

/// Swap a fully staged directory into place.
pub(super) fn publish(
    staging: &Path,
    published: &Path,
    backup: &Path,
) -> Result<PathBuf, AnalysisError> {
    reclaim_backup(published, backup)?;
    let had_published = published.exists();
    if had_published {
        fs::rename(published, backup).map_err(io_error)?;
    }
    if let Err(error) = fs::rename(staging, published) {
        if had_published {
            if let Err(restore) = fs::rename(backup, published) {
                warn!(
                    "Could not restore the previous report from {}: {restore}",
                    backup.display()
                );
            }
        }
        return Err(io_error(error));
    }
    if had_published {
        fs::remove_dir_all(backup).map_err(io_error)?;
    }
    Ok(published.to_path_buf())
}

/// Resolve a backup left behind by an interrupted publish: restore it when its published
/// counterpart is gone, and drop it when the publish did land. Without this an interrupted publish
/// would strand the last good report in the backup.
pub(super) fn reclaim_backup(published: &Path, backup: &Path) -> Result<(), AnalysisError> {
    if backup.exists() {
        if published.exists() {
            fs::remove_dir_all(backup).map_err(io_error)?;
        } else {
            fs::rename(backup, published).map_err(io_error)?;
        }
    }
    Ok(())
}

/// A staging directory left behind by an interrupted run is never a usable report.
pub(super) fn reset_staging(staging: &Path) -> Result<(), AnalysisError> {
    if staging.exists() {
        fs::remove_dir_all(staging).map_err(io_error)?;
    }
    fs::create_dir_all(staging).map_err(io_error)
}

#[cfg(test)]
mod tests {
    use super::{publish, reclaim_backup};
    use std::fs;

    #[test]
    fn failed_publish_restores_the_previous_report() {
        let root = tempfile::tempdir().unwrap();
        let published = root.path().join("analysis");
        let backup = root.path().join("backup");
        fs::create_dir(&published).unwrap();
        fs::write(published.join("index.html"), "previous report").unwrap();

        assert!(publish(&root.path().join("missing-staging"), &published, &backup).is_err());

        assert_eq!(
            fs::read_to_string(published.join("index.html")).unwrap(),
            "previous report"
        );
        assert!(!backup.exists());
    }

    #[test]
    fn reclaim_backup_restores_a_report_after_interrupted_publish() {
        let root = tempfile::tempdir().unwrap();
        let published = root.path().join("analysis");
        let backup = root.path().join("backup");
        fs::create_dir(&backup).unwrap();
        fs::write(backup.join("index.html"), "previous report").unwrap();

        reclaim_backup(&published, &backup).unwrap();

        assert_eq!(
            fs::read_to_string(published.join("index.html")).unwrap(),
            "previous report"
        );
        assert!(!backup.exists());
    }
}
