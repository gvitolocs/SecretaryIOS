//! What kind of document a Drive file is, from its name (and the catalog kind for media): the type the
//! apps show an icon for, the preview they open, and the syntax for code.

/// `drive.type` in the API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocType {
    Pdf,
    Document,
    Spreadsheet,
    Presentation,
    Text,
    Code,
    Archive,
    Image,
    Video,
    Audio,
    Other,
}

pub const ALL: [DocType; 11] = [
    DocType::Pdf,
    DocType::Document,
    DocType::Spreadsheet,
    DocType::Presentation,
    DocType::Text,
    DocType::Code,
    DocType::Archive,
    DocType::Image,
    DocType::Video,
    DocType::Audio,
    DocType::Other,
];

impl DocType {
    pub fn as_str(self) -> &'static str {
        match self {
            DocType::Pdf => "pdf",
            DocType::Document => "document",
            DocType::Spreadsheet => "spreadsheet",
            DocType::Presentation => "presentation",
            DocType::Text => "text",
            DocType::Code => "code",
            DocType::Archive => "archive",
            DocType::Image => "image",
            DocType::Video => "video",
            DocType::Audio => "audio",
            DocType::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        ALL.into_iter().find(|t| t.as_str() == s)
    }

    /// `drive.preview`: which viewer the apps open.
    pub fn preview(self) -> &'static str {
        match self {
            DocType::Pdf => "pdf",
            DocType::Document | DocType::Spreadsheet | DocType::Presentation => "office",
            DocType::Text => "text",
            DocType::Code => "code",
            DocType::Archive => "archive",
            DocType::Image => "image",
            DocType::Video => "video",
            DocType::Audio => "audio",
            DocType::Other => "none",
        }
    }

    /// Words added to the search text, so "slides", "slide" or "foglio" find the type.
    pub fn search_words(self) -> &'static str {
        match self {
            DocType::Pdf => "pdf document documento dokument",
            DocType::Document => "document documento dokument",
            DocType::Spreadsheet => "spreadsheet foglio calcolo regneark",
            DocType::Presentation => {
                "presentation slides slide presentazione diapositive præsentation dias"
            }
            DocType::Text => "text testo tekst notes",
            DocType::Code => "code codice kode",
            DocType::Archive => "archive archivio arkiv",
            _ => "",
        }
    }

    /// Whether the background worker extracts searchable text from this type.
    pub fn has_text(self) -> bool {
        matches!(
            self,
            DocType::Pdf
                | DocType::Document
                | DocType::Spreadsheet
                | DocType::Presentation
                | DocType::Text
                | DocType::Code
        )
    }
}

/// The lower-case extension of a file name (`archive.tar.gz` → `gz`), `""` when none.
pub fn ext_of(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && ext.len() <= 16 => {
            ext.to_ascii_lowercase()
        }
        _ => String::new(),
    }
}

/// The document type of a file called `name`; `kind` is the catalog kind (photo, video, audio, file).
pub fn classify(name: &str, kind: &str) -> DocType {
    let lower = name.to_ascii_lowercase();
    let ext = ext_of(&lower);
    match ext.as_str() {
        "pdf" => return DocType::Pdf,
        "doc" | "docx" | "docm" | "dot" | "dotx" | "odt" | "ott" | "rtf" | "pages" | "wpd" => {
            return DocType::Document
        }
        "xls" | "xlsx" | "xlsm" | "xlsb" | "ods" | "ots" | "numbers" => {
            return DocType::Spreadsheet
        }
        "ppt" | "pptx" | "pptm" | "pps" | "ppsx" | "odp" | "otp" | "key" => {
            return DocType::Presentation
        }
        "zip" | "tar" | "tgz" | "gz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "jar" | "apk"
        | "war" | "tbz" | "tbz2" | "txz" => return DocType::Archive,
        _ => {}
    }
    if language_for(&lower).is_some() {
        return if is_prose(&lower) {
            DocType::Text
        } else {
            DocType::Code
        };
    }
    match kind {
        "photo" => DocType::Image,
        "video" => DocType::Video,
        "audio" => DocType::Audio,
        _ => DocType::Other,
    }
}

/// Text meant to be read rather than run.
fn is_prose(lower: &str) -> bool {
    matches!(
        ext_of(lower).as_str(),
        "txt"
            | "md"
            | "markdown"
            | "rst"
            | "org"
            | "log"
            | "csv"
            | "tsv"
            | "tex"
            | "bib"
            | "srt"
            | "vtt"
    ) || matches!(
        lower,
        "readme" | "license" | "licence" | "changelog" | "authors" | "notice"
    )
}

/// The syntax name for a text or code file (the name syntect knows it by), from its name.
pub fn language_for(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let base = lower.rsplit('/').next().unwrap_or(&lower);
    let by_name = match base {
        "makefile" | "gnumakefile" => Some("Makefile"),
        "dockerfile" | "containerfile" => Some("Dockerfile"),
        "cmakelists.txt" => Some("CMake"),
        "cargo.lock" | "pipfile" => Some("TOML"),
        "gemfile" | "rakefile" => Some("Ruby"),
        "readme" | "license" | "licence" | "changelog" | "authors" | "notice" => Some("Plain Text"),
        ".gitignore" | ".dockerignore" | ".editorconfig" => Some("Plain Text"),
        _ => None,
    };
    if by_name.is_some() {
        return by_name;
    }
    Some(match ext_of(base).as_str() {
        "txt" | "log" | "srt" | "vtt" | "org" => "Plain Text",
        "md" | "markdown" => "Markdown",
        "rst" => "reStructuredText",
        "csv" | "tsv" => "CSV",
        "tex" | "sty" | "cls" => "LaTeX",
        "bib" => "BibTeX",
        "rs" => "Rust",
        "py" | "pyw" | "pyi" => "Python",
        "ipynb" | "json" | "geojson" | "jsonl" | "webmanifest" => "JSON",
        "js" | "mjs" | "cjs" => "JavaScript",
        "jsx" => "JavaScript (Babel)",
        "ts" | "mts" | "cts" => "TypeScript",
        "tsx" => "TypeScriptReact",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "ino" => "C++",
        "cs" => "C#",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "scala" | "sc" => "Scala",
        "swift" => "Swift",
        "go" => "Go",
        "rb" => "Ruby",
        "php" => "PHP",
        "pl" | "pm" => "Perl",
        "lua" => "Lua",
        "r" => "R",
        "m" | "mm" => "Objective-C",
        "hs" => "Haskell",
        "ml" | "mli" => "OCaml",
        "erl" | "hrl" => "Erlang",
        "ex" | "exs" => "Elixir",
        "clj" | "cljs" | "edn" => "Clojure",
        "dart" => "Dart",
        "groovy" | "gradle" => "Groovy",
        "sh" | "bash" | "zsh" | "ksh" | "fish" | "envrc" => "Bourne Again Shell (bash)",
        "bat" | "cmd" => "Batch File",
        "ps1" | "psm1" => "PowerShell",
        "sql" => "SQL",
        "html" | "htm" | "xhtml" | "vue" | "svelte" => "HTML",
        "css" => "CSS",
        "scss" | "sass" | "less" => "CSS",
        "xml" | "xsd" | "xsl" | "svg" | "plist" | "csproj" | "fsproj" | "props" | "kml" | "gpx" => {
            "XML"
        }
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        "ini" | "cfg" | "conf" | "properties" | "env" => "INI",
        "diff" | "patch" => "Diff",
        "proto" | "graphql" | "gql" | "zig" | "nim" | "jl" | "f90" | "f" | "v" | "sv" | "vhd"
        | "asm" | "s" | "cmake" | "nix" | "tf" | "hcl" | "sol" | "cu" | "glsl" | "wgsl" => {
            "Plain Text"
        }
        "d" => "D",
        "pas" => "Pascal",
        "lisp" | "el" | "scm" => "Lisp",
        "tcl" => "Tcl",
        "mat" | "matlab" => "MATLAB",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_the_owners_files() {
        assert_eq!(
            classify("L05 - Digital Forensics.pdf", "file"),
            DocType::Pdf
        );
        assert_eq!(classify("Slides.PPTX", "file"), DocType::Presentation);
        assert_eq!(classify("notes.odt", "file"), DocType::Document);
        assert_eq!(classify("grades.xlsx", "file"), DocType::Spreadsheet);
        assert_eq!(classify("data.csv", "file"), DocType::Text);
        assert_eq!(classify("README.md", "file"), DocType::Text);
        assert_eq!(classify("main.rs", "file"), DocType::Code);
        assert_eq!(classify("Makefile", "file"), DocType::Code);
        assert_eq!(classify("trace.tar.gz", "file"), DocType::Archive);
        assert_eq!(classify("lecture.mp4", "video"), DocType::Video);
        assert_eq!(classify("IMG_1.HEIC", "photo"), DocType::Image);
        assert_eq!(classify("blob.bin", "file"), DocType::Other);
        assert_eq!(
            classify(".pdf", "file"),
            DocType::Other,
            "a dot file has no extension"
        );
        assert_eq!(classify("noext", "file"), DocType::Other);
    }

    #[test]
    fn types_round_trip_and_preview() {
        for t in ALL {
            assert_eq!(DocType::parse(t.as_str()), Some(t));
        }
        assert_eq!(DocType::parse("exe"), None);
        assert_eq!(DocType::Presentation.preview(), "office");
        assert_eq!(DocType::Other.preview(), "none");
        assert!(DocType::Pdf.has_text() && !DocType::Archive.has_text());
    }

    #[test]
    fn languages() {
        assert_eq!(language_for("src/main.rs"), Some("Rust"));
        assert_eq!(language_for("Dockerfile"), Some("Dockerfile"));
        assert_eq!(language_for("a.unknownext"), None);
        assert_eq!(ext_of("archive.tar.gz"), "gz");
        assert_eq!(ext_of("x."), "");
    }
}
