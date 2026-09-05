use crate::snapshot::Kind;

/// RGB triple used by the native UI. Kept in core so the daemon snapshot
/// can carry a stable colour without depending on egui.
pub fn rgb_for_kind(kind: &Kind, name: &str) -> (u8, u8, u8) {
    match kind {
        Kind::Free => (36, 40, 48),
        Kind::Overhead => (64, 56, 46),
        Kind::Scanning => (86, 92, 58),
        Kind::Other => (92, 96, 108),
        Kind::Unmounted => (48, 52, 62),
        Kind::Locked => (72, 42, 48),
        Kind::Swap => (58, 64, 52),
        Kind::Directory => (62, 88, 122),
        Kind::MountPoint { .. } => (48, 148, 168),
        Kind::File { ext } => rgb_for_ext(ext, name),
    }
}

pub fn rgb_for_ext(ext: &str, name: &str) -> (u8, u8, u8) {
    let ext = ext.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "svg" | "bmp" | "tif" | "tiff" | "heic"
        | "raw" | "cr2" | "nef" | "ico" | "avif" => (176, 86, 168),
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "m4v" | "wmv" | "flv" | "mpeg" | "mpg"
        | "m2ts" | "mts" | "vob" => (196, 64, 72),
        "mp3" | "flac" | "wav" | "ogg" | "aac" | "m4a" | "wma" | "aiff" | "opus" => {
            (196, 168, 52)
        }
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "zst" | "lz4" | "iso"
        | "img" | "dmg" => (204, 116, 48),
        "pdf" | "doc" | "docx" | "odt" | "rtf" | "txt" | "md" | "epub" | "xls" | "xlsx"
        | "ods" | "ppt" | "pptx" | "csv" => (72, 128, 196),
        "rs" | "c" | "h" | "cc" | "cpp" | "hpp" | "js" | "ts" | "jsx" | "tsx" | "py" | "go"
        | "java" | "kt" | "swift" | "rb" | "php" | "sh" | "bash" | "zsh" | "lua" | "toml"
        | "yaml" | "yml" | "json" | "xml" | "html" | "css" | "scss" | "vue" | "svelte" => {
            (64, 160, 112)
        }
        "ttf" | "otf" | "woff" | "woff2" => (128, 112, 176),
        "exe" | "dll" | "so" | "o" | "a" | "dylib" | "wasm" | "bin" | "elf" => (140, 148, 156),
        "db" | "sqlite" | "sqlite3" | "sql" | "mdb" => (88, 140, 148),
        "pacman" | "pkg.tar.zst" => (96, 148, 176),
        _ if name.ends_with(".pkg.tar.zst") || name.ends_with(".pkg.tar.xz") => (96, 148, 176),
        _ => (158, 162, 174),
    }
}
