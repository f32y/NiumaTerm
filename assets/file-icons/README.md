# File Icons

Selected SVG assets from https://github.com/file-icons/icons.

- Upstream revision: `e6e6e6ac8cb1d91867167c228c00a667f4d47101`
- Source directory: `svg/`
- License: ISC; see [LICENSE](LICENSE).
- SVG files and filenames are preserved as downloaded.

## Included groups

| Group | Icons |
| --- | --- |
| Languages | C++, C#, TypeScript, Go, PHP, Kotlin, Lua, Zig |
| Web | Vue |
| Data and settings | JSON-1, JSON5, YAML, TOML, Config, SQLite |
| Language settings | Config-Rust, Config-Python, Config-JS, Config-Ruby, Config-TypeScript |
| Build and tools | Docker, Cmake, PowerShell, Terminal, EditorConfig, NPM |
| Documents and media | Adobe-Acrobat, Microsoft-Word, Microsoft-Excel, Microsoft-PowerPoint, Image |
| Fallback | Default |

The `Config-*` assets depict language settings. They are not the standalone
language logos. This collection does not yet include standalone Rust, Python,
JavaScript, or Markdown icons.

These assets can be tinted as a whole using GPUI's `svg().text_color(...)`.
The application embeds these files through `AppAssets`. Transcript file links
select an icon by filename or extension, with `Default.svg` for other files.
