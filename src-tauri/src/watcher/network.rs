//! Whether a folder is on a network drive, where change events cannot be
//! relied on.
//!
//! On Linux, inotify sees nothing that happens on NFS, SMB or most FUSE mounts:
//! the watch starts, and never fires. On Windows, change notification over SMB
//! depends on the server, and a burst overflows it without saying so. Such a
//! folder is scanned on a timer instead.
//!
//! The helpers below are plain string work, compiled on every platform so they
//! are tested on every platform, whichever one actually calls them.

use std::path::Path;

pub fn is_network_path(path: &Path) -> bool {
    #[cfg(windows)]
    let network = is_unc(path);

    #[cfg(target_os = "linux")]
    let network = std::fs::read_to_string("/proc/self/mountinfo")
        .ok()
        .and_then(|table| fs_type_for(&table, path))
        .is_some_and(|fs| is_network_fs(&fs));

    // macOS is not a platform Firesync ships for; a development build there
    // treats every folder as local.
    #[cfg(not(any(windows, target_os = "linux")))]
    let network = {
        let _ = path;
        false
    };

    network
}

/// `\\server\share\…`, in any of its spellings. A mapped drive letter is
/// stored this way too: a folder is resolved when it is added, and resolving a
/// mapped drive gives its UNC path.
#[cfg_attr(not(windows), allow(dead_code))]
fn is_unc(path: &Path) -> bool {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return rest.len() >= 4 && rest[..4].eq_ignore_ascii_case(r"UNC\");
    }
    text.starts_with(r"\\") && !text.starts_with(r"\\.\")
}

/// Undo the octal escapes mountinfo uses for spaces, tabs, newlines and
/// backslashes in a mount point.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn unescape(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let digits: String = chars.clone().take(3).collect();
            if digits.len() == 3 && digits.chars().all(|d| ('0'..='7').contains(&d)) {
                if let Ok(code) = u8::from_str_radix(&digits, 8) {
                    out.push(code as char);
                    for _ in 0..3 {
                        chars.next();
                    }
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

/// The filesystem type of whatever `path` is mounted on, from the text of
/// `/proc/self/mountinfo`: the entry with the longest mount point that
/// contains it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn fs_type_for(mountinfo: &str, path: &Path) -> Option<String> {
    let path = path.to_string_lossy();
    let mut best: Option<(usize, String)> = None;
    for line in mountinfo.lines() {
        // `id parent major:minor root mount-point options [optional…] - type source super-options`
        let Some((left, right)) = line.split_once(" - ") else { continue };
        let Some(mount_point) = left.split(' ').nth(4).map(unescape) else { continue };
        let Some(fs_type) = right.split(' ').next() else { continue };
        let contains = mount_point == "/"
            || path == mount_point.as_str()
            || path.strip_prefix(mount_point.as_str()).is_some_and(|rest| rest.starts_with('/'));
        if contains && best.as_ref().is_none_or(|(len, _)| mount_point.len() > *len) {
            best = Some((mount_point.len(), fs_type.to_string()));
        }
    }
    best.map(|(_, fs_type)| fs_type)
}

/// Filesystems whose changes inotify does not see.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_network_fs(fs_type: &str) -> bool {
    matches!(
        fs_type,
        "nfs" | "nfs4" | "cifs" | "smb3" | "smbfs" | "9p" | "afs" | "ceph" | "glusterfs" | "davfs"
    ) || fs_type.starts_with("fuse")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unc_paths_are_network_paths_in_every_spelling() {
        assert!(is_unc(Path::new(r"\\nas\recordings\clips")));
        assert!(is_unc(Path::new(r"\\?\UNC\nas\recordings")));
        assert!(!is_unc(Path::new(r"\\?\C:\Recordings")), "a long local path");
        assert!(!is_unc(Path::new(r"\\.\PhysicalDrive0")), "a device");
        assert!(!is_unc(Path::new(r"E:\Recordings")));
    }

    const MOUNTINFO: &str = "\
22 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw
40 22 0:44 / /mnt/nas rw,relatime shared:20 - cifs //nas/recordings rw,vers=3.1.1
41 22 0:45 / /mnt/nas2 rw,relatime shared:21 - ext4 /dev/sdb1 rw
42 22 8:17 / /media/shane/Game\\040Clips rw,nosuid shared:22 - vfat /dev/sdc1 rw
43 22 0:50 / /run/user/1000/gvfs rw,nosuid shared:23 - fuse.gvfsd-fuse gvfsd-fuse rw
44 22 0:51 / /mnt/wsl-share rw shared:24 - 9p drvfs rw
";

    #[test]
    fn the_longest_containing_mount_point_decides() {
        assert_eq!(fs_type_for(MOUNTINFO, Path::new("/mnt/nas/clips/VALORANT")).as_deref(), Some("cifs"));
        assert_eq!(fs_type_for(MOUNTINFO, Path::new("/mnt/nas")).as_deref(), Some("cifs"));
        assert_eq!(fs_type_for(MOUNTINFO, Path::new("/home/shane/Videos")).as_deref(), Some("ext4"));
    }

    #[test]
    fn a_mount_point_is_not_matched_inside_a_longer_name() {
        assert_eq!(fs_type_for(MOUNTINFO, Path::new("/mnt/nas2/clips")).as_deref(), Some("ext4"));
    }

    #[test]
    fn escaped_spaces_in_mount_points_are_understood() {
        assert_eq!(
            fs_type_for(MOUNTINFO, Path::new("/media/shane/Game Clips/VALORANT")).as_deref(),
            Some("vfat")
        );
    }

    #[test]
    fn network_and_fuse_filesystems_count_and_local_ones_do_not() {
        for fs in ["cifs", "nfs4", "smb3", "9p", "fuse.gvfsd-fuse", "fuse.sshfs"] {
            assert!(is_network_fs(fs), "{fs}");
        }
        for fs in ["ext4", "btrfs", "xfs", "vfat", "ntfs3", "exfat"] {
            assert!(!is_network_fs(fs), "{fs}");
        }
    }
}
