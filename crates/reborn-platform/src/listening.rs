//! The listening stats' record on disk: one JSON line per listen in
//! `/data/reborn/state/listening.jsonl`, appended as listens end. Nothing
//! leaves the player. A line that can't be read (a write cut off by power
//! loss) is skipped; Reset removes the file.

use reborn_core::stats::Listen;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub fn path(root: &Path) -> PathBuf {
    root.join("state/listening.jsonl")
}

/// Every listen kept, oldest first.
pub fn load(path: &Path) -> Vec<Listen> {
    let Ok(file) = fs::File::open(path) else {
        return vec![];
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect()
}

/// Keep one more listen (the file is only ever appended to).
pub fn append(path: &Path, listen: &Listen) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_vec(listen).map_err(io::Error::other)?;
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    // A line cut off by power loss stays alone on its line.
    if file.seek(SeekFrom::End(0))? > 0 {
        let mut last = [0u8];
        file.seek(SeekFrom::End(-1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            line.insert(0, b'\n');
        }
    }
    // One write per line: a cut-off write damages at most that line.
    file.write_all(&line)
}

/// Forget every listen.
pub fn clear(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reborn_core::stats::Source;

    #[test]
    fn listens_append_survive_a_cut_line_and_clear() {
        let dir = std::env::temp_dir().join(format!("reborn-listening-{}", std::process::id()));
        let file = path(&dir);
        let listen = |at| Listen {
            at,
            ms: 60_000,
            title: "Song".into(),
            artist: "Artist".into(),
            album: String::new(),
            album_artist: String::new(),
            source: Source::Library,
        };
        append(&file, &listen(1)).unwrap();
        append(&file, &listen(2)).unwrap();
        // Power lost in the middle of a third write.
        OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap()
            .write_all(b"{\"t\":3,\"ms\"")
            .unwrap();
        assert_eq!(load(&file), vec![listen(1), listen(2)]);
        // The next listen starts a line of its own.
        append(&file, &listen(4)).unwrap();
        assert_eq!(load(&file), vec![listen(1), listen(2), listen(4)]);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        clear(&file).unwrap();
        assert!(load(&file).is_empty());
        clear(&file).unwrap();
        let _ = fs::remove_dir_all(dir);
    }
}
