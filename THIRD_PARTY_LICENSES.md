# Third-Party Licenses

The plugins' own code is under the MIT license in [`LICENSE`](LICENSE). The plugin
archives also ship the third-party assets below, under MIT, BSD-3-Clause, OFL 1.1, the
Ubuntu Font Licence 1.0, and the Bitstream Vera license.

| Project | Version | License | Included in | License text |
|---|---|---|---|---|
| mermaid | 11.16.1 | MIT | markdown plugin binary | [`LICENSES/mermaid-MIT.txt`](LICENSES/mermaid-MIT.txt) |
| highlight.js | 11.10.0 | BSD-3-Clause | markdown plugin binary | [`LICENSES/highlightjs-BSD-3-Clause.txt`](LICENSES/highlightjs-BSD-3-Clause.txt) |
| KaTeX | 0.18.4 | MIT | markdown plugin binary (JS, CSS, 20 fonts) | [`LICENSES/KaTeX-MIT.txt`](LICENSES/KaTeX-MIT.txt) |
| egui default fonts: Hack Regular | epaint_default_fonts 0.31.1 | MIT, Bitstream Vera | every plugin binary | [`LICENSES/Hack-MIT-Bitstream-Vera.txt`](LICENSES/Hack-MIT-Bitstream-Vera.txt) |
| egui default fonts: Noto Emoji Regular | epaint_default_fonts 0.31.1 | OFL 1.1 | every plugin binary | [`LICENSES/NotoEmoji-OFL-1.1.txt`](LICENSES/NotoEmoji-OFL-1.1.txt) |
| egui default fonts: Ubuntu Light | epaint_default_fonts 0.31.1 | Ubuntu Font Licence 1.0 | every plugin binary | [`LICENSES/Ubuntu-Ubuntu-font-1.0.txt`](LICENSES/Ubuntu-Ubuntu-font-1.0.txt) |
| egui default fonts: emoji-icon-font | epaint_default_fonts 0.31.1 | MIT | every plugin binary | [`LICENSES/emoji-icon-font-MIT.txt`](LICENSES/emoji-icon-font-MIT.txt) |

The files in `LICENSES/` are copied unchanged. The mermaid, highlight.js, and KaTeX texts
are the `LICENSE` files at the tags of the bundled versions, the same files tasty ships.
The font texts are the license files in the `fonts/` folder of the `epaint_default_fonts`
0.31.1 crate that `Cargo.lock` locks.

## markdown plugin rendering engines

The markdown plugin (`crates/tasty-plugin-markdown`) compiles the files of these three
engines into its binary with `include_str!` and `include_bytes!`, and does not download
them at run time. The sources, sha512 hashes, and how to update them are in
[the asset notice](crates/tasty-plugin-markdown/assets/NOTICE.md), which the markdown
archive also ships as `NOTICE.md`.

### mermaid

- Source: https://github.com/mermaid-js/mermaid (npm `mermaid`)
- Version: 11.16.1
- Applies to: `crates/tasty-plugin-markdown/assets/mermaid.min.js`
- License: MIT, Copyright (c) 2014 - 2022 Knut Sveidqvist
- License text: [`LICENSES/mermaid-MIT.txt`](LICENSES/mermaid-MIT.txt), the `LICENSE` at the
  tag `mermaid@11.16.1`
- The notices of other MIT components inside the bundle are kept in `mermaid.min.js`.

### highlight.js

- Source: https://github.com/highlightjs/highlight.js (the prebuilt "common" bundle on cdnjs)
- Version: 11.10.0
- Applies to: `crates/tasty-plugin-markdown/assets/highlight.min.js`
- License: BSD-3-Clause, Copyright (c) 2006, Ivan Sagalaev
- License text: [`LICENSES/highlightjs-BSD-3-Clause.txt`](LICENSES/highlightjs-BSD-3-Clause.txt),
  the `LICENSE` at the tag `11.10.0`
- The banner at the head of the bundle is kept unchanged.

### KaTeX

- Source: https://github.com/KaTeX/KaTeX (npm `katex`)
- Version: 0.18.4
- Applies to: `crates/tasty-plugin-markdown/assets/katex.min.js`,
  `crates/tasty-plugin-markdown/assets/katex.min.css`, and the 20
  `crates/tasty-plugin-markdown/assets/fonts/KaTeX_*.woff2` fonts
- License: MIT, Copyright (c) 2013-2020 Khan Academy and other contributors
- License text: [`LICENSES/KaTeX-MIT.txt`](LICENSES/KaTeX-MIT.txt), the `LICENSE` at the tag
  `v0.18.4`. The same license covers the fonts.

## egui default fonts

Every plugin links egui's `epaint` with its `default_fonts` feature, so the four fonts of
the `epaint_default_fonts` crate (https://github.com/emilk/egui, version 0.31.1) are
compiled into every plugin binary.

- Hack Regular (`Hack-Regular.ttf`, https://github.com/source-foundry/Hack): MIT for the Hack
  work, and the Bitstream Vera license with the Reserved Font Names "Bitstream" and "Vera".
  Text: [`LICENSES/Hack-MIT-Bitstream-Vera.txt`](LICENSES/Hack-MIT-Bitstream-Vera.txt).
- Noto Emoji Regular (`NotoEmoji-Regular.ttf`): SIL Open Font License 1.1.
  Text: [`LICENSES/NotoEmoji-OFL-1.1.txt`](LICENSES/NotoEmoji-OFL-1.1.txt).
- Ubuntu Light (`Ubuntu-Light.ttf`): Ubuntu Font Licence 1.0.
  Text: [`LICENSES/Ubuntu-Ubuntu-font-1.0.txt`](LICENSES/Ubuntu-Ubuntu-font-1.0.txt).
- emoji-icon-font (`emoji-icon-font.ttf`, https://github.com/jslegers/emoji-icon-font): MIT,
  Copyright (c) 2014 John Slegers.
  Text: [`LICENSES/emoji-icon-font-MIT.txt`](LICENSES/emoji-icon-font-MIT.txt).

The crate embeds the font files with `include_bytes!`.

## Notice set

Every plugin archive holds these files next to the plugin, copied from this repository
and not generated at release time:

- `LICENSE`: the MIT text of the plugins' own code
- `THIRD_PARTY_LICENSES.md`: this file
- every file in `LICENSES/`, under `LICENSES/`
- `NOTICE.md`: the plugin's `assets/NOTICE.md`, for a plugin that has one (now markdown only)

A new bundled asset adds its license text to `LICENSES/` and a row to the table above.

## Limits

This notice set does not complete the notices of every statically linked Rust
dependency. The plugin binaries also link crates.io and git crates under MIT, Apache-2.0,
MPL-2.0, Unicode-3.0, BSL-1.0, Zlib, and other licenses, whose notices are not collected
here.
