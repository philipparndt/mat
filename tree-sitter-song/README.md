# tree-sitter-song

A [tree-sitter](https://tree-sitter.github.io) grammar for musik-as-text
`.song` files (see `../docs/FORMAT.md`), for editors to colour them.

- C entry point: `const TSLanguage *tree_sitter_song(void)` in `src/parser.c`
- ABI 15, generated with tree-sitter CLI 0.25.10; no external scanner
- Highlights: `queries/highlights.scm`

To use it, compile `src/parser.c` (with `src/tree_sitter/*.h` on the include
path) and load `queries/highlights.scm`.

## Regenerate and test

```sh
tree-sitter generate     # grammar.js -> src/
tree-sitter test         # test/corpus and test/highlight
tree-sitter parse -q ../examples/*.song ../crates/mat-core/presets.song
```

`tree-sitter parse -q` prints nothing when every file parses without an
`ERROR` or `MISSING` node.

## Shape

Layout is carried by two tokens: a newline followed by column 0 ends a line,
one followed by indentation starts a body line. A column-0 comment between body
lines does not end the block.

| Block | Node | Body lines |
|---|---|---|
| `title` `tempo` `meter` `swing` `seed` | `statement` | `setting` |
| `include "<file>"` | `include_statement` (`path`) | `setting` |
| `section <name>` | `section_block` | `setting` |
| `instrument <name> <kind>` / `preset <preset>` | `instrument_block` | `setting`, `reference_setting` |
| `preset <name> <kind> "…"` (presets.song) | `preset_block` | `setting` |
| `pattern <name> [grid=…] [bars=…] [pedal]` | `pattern_block` | `note_line`, `grid_row` |
| `track <name>` | `track_block` | `setting`, `reference_setting`, `play_step`, `sweep` |
| `master` / `master preset <preset>` | `master_block` | `setting`, `reference_setting` |
| any other word | `unknown_block` | `setting` |

- `setting`: `setting_name` then items: `option` (`key` `=` value), `number`
  (units `ms` `s` `dB` `Hz` `k`), `fraction` (`3/16`), `range` (`17-32`,
  `0.5s-1.7s`), `note`, `note_range` (`C1-B3`), `string`, `identifier`.
- `reference_setting`: `instrument`, `sidechain`, `layer`, `source` with a
  `reference` to another block.
- `play_step`: `play` with a `reference` to a pattern, `repeat_count` (`x2`)
  and options.
- `note_line`: `event`s — `pitch` (`note`, `drum`, `rest`, `chord`),
  `duration` after `:`, `velocity` after `@`, `articulation` (`!` `~`) — and
  `bar` (`|`).
- `grid_row`: a `drum` or `note` voice, then runs of `grid_hit` (`x X o 1-9`),
  `grid_hold` (`=`) and `grid_rest` (`. - _`).

The grammar is lenient on purpose: any line takes unknown words, and characters
it cannot place become a `word` node instead of an error, so a half-typed line
does not colour the rest of the file as broken. It does not check what the
`mat` parser checks (known settings, option names, bar lengths).
