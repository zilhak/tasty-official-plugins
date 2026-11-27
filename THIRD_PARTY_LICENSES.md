# Third-Party Licenses

The plugins' own code is under the MIT license in [`LICENSE`](LICENSE). The plugin
archives also ship the third-party assets below, under MIT, BSD-3-Clause, ISC, OFL 1.1, the
Ubuntu Font Licence 1.0, and the Bitstream Vera license.

| Project | Version | License | Included in | License text |
|---|---|---|---|---|
| mermaid | 11.16.1 | MIT | markdown plugin binary | [`LICENSES/mermaid-MIT.txt`](LICENSES/mermaid-MIT.txt) |
| highlight.js | 11.10.0 | BSD-3-Clause | markdown plugin binary | [`LICENSES/highlightjs-BSD-3-Clause.txt`](LICENSES/highlightjs-BSD-3-Clause.txt) |
| KaTeX | 0.18.4 | MIT | markdown plugin binary (JS, CSS, 20 fonts) | [`LICENSES/KaTeX-MIT.txt`](LICENSES/KaTeX-MIT.txt) |
| egui default fonts: Hack Regular | epaint_default_fonts 0.31.1 | MIT, Bitstream Vera | clipboard-viewer, git-viewer, image, and markdown plugin binaries | [`LICENSES/Hack-MIT-Bitstream-Vera.txt`](LICENSES/Hack-MIT-Bitstream-Vera.txt) |
| egui default fonts: Noto Emoji Regular | epaint_default_fonts 0.31.1 | OFL 1.1 | clipboard-viewer, git-viewer, image, and markdown plugin binaries | [`LICENSES/NotoEmoji-OFL-1.1.txt`](LICENSES/NotoEmoji-OFL-1.1.txt) |
| egui default fonts: Ubuntu Light | epaint_default_fonts 0.31.1 | Ubuntu Font Licence 1.0 | clipboard-viewer, git-viewer, image, and markdown plugin binaries | [`LICENSES/Ubuntu-Ubuntu-font-1.0.txt`](LICENSES/Ubuntu-Ubuntu-font-1.0.txt) |
| egui default fonts: emoji-icon-font | epaint_default_fonts 0.31.1 | MIT | clipboard-viewer, git-viewer, image, and markdown plugin binaries | [`LICENSES/emoji-icon-font-MIT.txt`](LICENSES/emoji-icon-font-MIT.txt) |
| Lucide (including parts from Feather) | lucide-static 0.460.0 | ISC, MIT (Feather) | clipboard-viewer, git-viewer, image, and markdown plugins (those whose own normal and build dependencies include tasty-icons) | [`LICENSES/Lucide-ISC.txt`](LICENSES/Lucide-ISC.txt), [`LICENSES/Feather-MIT.txt`](LICENSES/Feather-MIT.txt) |

The files in `LICENSES/` are copied unchanged. The mermaid, highlight.js, and KaTeX texts
are the `LICENSE` files at the tags of the bundled versions, the same files tasty ships.
The font texts are the license files in the `fonts/` folder of the `epaint_default_fonts`
0.31.1 crate that `Cargo.lock` locks. The Lucide and Feather texts are byte for byte the
files of the same names in tasty's `LICENSES/`; their sources are in the icons section below.

The Lucide row lists the plugins whose own normal and build dependencies, resolved with that
plugin's features alone, include `tasty-icons`. A release builds all plugins in one cargo
build, where features are unified, so every plugin compiles `tasty-icons` there; the row
does not claim which icon shapes stay in each final binary.

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

The clipboard-viewer, git-viewer, image, and markdown plugins link egui's `epaint` with its
`default_fonts` feature, so the four fonts of the `epaint_default_fonts` crate
(https://github.com/emilk/egui, version 0.31.1) are compiled into those four plugin
binaries. The other plugins do not link `epaint`. The notice set below still goes into
every plugin archive.

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

## Icons

### Lucide (including parts from Feather)

- Source: Lucide Contributors, https://github.com/lucide-icons/lucide (npm `lucide-static`).
  Parts of Lucide come from Feather (https://github.com/feathericons/feather, Cole Bemis).
- Version: the Version column of the Lucide row in the table above.
- Applies to: every icon definition in tasty's `tasty-icons` crate whose shape is the same
  as a Lucide icon (including the parts from Feather) or was changed from one. Which
  definitions those are is recorded by the source comments in tasty's
  `crates/tasty-icons/src/lib.rs` and in tasty's `THIRD_PARTY_LICENSES.md`; this file does
  not list them. The plugins reach those shapes in three ways: the clipboard-viewer and
  image `build.rs` scripts bake icon shapes into the binary, the markdown plugin uses them
  in its `build.rs` and its renderer, and git-viewer links `tasty-icons` through
  `tasty-ui-widgets`.
- License: ISC for the Lucide parts, MIT for the Feather parts.
- License texts:
  - [`LICENSES/Lucide-ISC.txt`](LICENSES/Lucide-ISC.txt): the `LICENSE` of the npm
    `lucide-static` package at the version above (the same as the `LICENSE` at the lucide
    repository tag of that version), sha256
    `1e7290b35280a048667bbf0ebabac1c7fd52a75300e8b2946ac165715997f2bc`.
  - [`LICENSES/Feather-MIT.txt`](LICENSES/Feather-MIT.txt): the `LICENSE` at
    feathericons/feather commit `3dc050d97405062eba78aa57115c0a15c63abdaa`, sha256
    `308028e93fcf84972523cdf6e616f73168546b4953895f516d01287f16fe7bee`. Lucide's
    `LICENSE` names the copyright of the Feather parts but does not carry the MIT
    permission text, so the Feather text is shipped as well.
- Copyright: as in the copyright lines at the head of those two files.

The two licenses word their notice condition differently. ISC (`LICENSES/Lucide-ISC.txt`)
requires that "the above copyright notice and this permission notice appear in all
copies", and MIT (`LICENSES/Feather-MIT.txt`) requires that "The above copyright notice
and this permission notice shall be included in all copies or substantial portions of the
Software."

The other plugins do not have `tasty-icons` in their own normal and build dependencies. The
notice set below still goes into every plugin archive.

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
