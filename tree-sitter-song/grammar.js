/**
 * @file tree-sitter grammar for musik-as-text `.song` files
 * @license MIT
 *
 * The format is line based: a block header starts at column 0 and the indented
 * lines below it are its body (see docs/FORMAT.md). Two hidden tokens carry the
 * layout without an external scanner:
 *
 *   _line_break  a newline (plus blank lines) followed by column 0
 *   _indent      a newline (plus blank lines) followed by indentation
 *
 * Both are matched by longest match, so "\n  \n" is a line break and
 * "\n  \n  kick" is an indent.
 *
 * The grammar is deliberately lenient: every line accepts any word, number or
 * stray character, so a half-typed line never turns the rest of a file into
 * an error.
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

const NOTE = '[A-Ga-g][#b]*-?[0-9]+';
const NUM = '[+-]?([0-9]+(\\.[0-9]*)?|\\.[0-9]+)';

module.exports = grammar({
  name: 'song',

  extras: $ => [/[ \t\r\f]+/, $.comment],

  word: $ => $.identifier,

  // A line break after a block either ends it or, when an indented line
  // follows (a column-0 comment in between), continues its body.
  conflicts: $ => [
    [$.statement],
    [$.include_statement],
    [$.section_block],
    [$.instrument_block],
    [$.preset_block],
    [$.pattern_block],
    [$.track_block],
    [$.master_block],
    [$.unknown_block],
  ],

  rules: {
    source_file: $ => seq(
      optional($._unit),
      repeat(seq($._line_break, optional($._unit))),
    ),

    _unit: $ => choice(
      $.statement,
      $.include_statement,
      $.section_block,
      $.instrument_block,
      $.preset_block,
      $.pattern_block,
      $.track_block,
      $.master_block,
      $.unknown_block,
      $._orphan_lines,
    ),

    // Indented lines before any block.
    _orphan_lines: $ => repeat1(seq($._indent, optional($._setting_line))),

    // ------------------------------------------------------------ blocks

    // title / tempo / meter / swing / seed
    statement: $ => seq(
      field('keyword', choice('title', 'tempo', 'meter', 'swing', 'seed')),
      repeat($._item),
      body($, $._setting_line),
    ),

    // include "kits/drums.song"
    include_statement: $ => seq(
      'include',
      optional(choice(
        seq(field('path', $.string), repeat($._item)),
        // An unquoted path, half typed: not an error here, the parser says so.
        seq(
          choice($.identifier, $.option, $.number, $.fraction, $.range, $.note, $.note_range, $.repeat_count, $.bar, $._junk),
          repeat($._item),
        ),
      )),
      body($, $._setting_line),
    ),

    section_block: $ => seq(
      'section',
      after($, field('name', alias($.identifier, $.name))),
      body($, $._setting_line),
    ),

    // instrument <name> <kind> | instrument <name> preset <preset>
    instrument_block: $ => seq(
      'instrument',
      optional(choice(
        seq(
          field('name', alias($.identifier, $.name)),
          optional(choice(
            seq('preset', after($, field('preset', alias($.identifier, $.preset_name)))),
            seq(field('kind', alias($.identifier, $.instrument_kind)), repeat($._item)),
            seq($._plain_item, repeat($._item)),
          )),
        ),
        seq($._plain_item, repeat($._item)),
      )),
      body($, $._setting_line),
    ),

    // presets.song: preset <name> <kind> "description"
    preset_block: $ => seq(
      'preset',
      optional(choice(
        seq(
          field('name', alias($.identifier, $.name)),
          optional(choice(
            seq(field('kind', alias($.identifier, $.instrument_kind)), repeat($._item)),
            seq($._plain_item, repeat($._item)),
          )),
        ),
        seq($._plain_item, repeat($._item)),
      )),
      body($, $._setting_line),
    ),

    pattern_block: $ => seq(
      'pattern',
      after($, field('name', alias($.identifier, $.name))),
      body($, $._pattern_line),
    ),

    track_block: $ => seq(
      'track',
      after($, field('name', alias($.identifier, $.name))),
      body($, $._track_line),
    ),

    master_block: $ => seq(
      'master',
      choice(
        seq('preset', after($, field('preset', alias($.identifier, $.preset_name)))),
        repeat($._item),
      ),
      body($, $._setting_line),
    ),

    // Anything else at column 0: an unknown or half-typed keyword.
    unknown_block: $ => seq(
      choice($.identifier, $._plain_item),
      repeat($._item),
      body($, $._setting_line),
    ),

    // ------------------------------------------------------------ settings

    _setting_line: $ => choice($.setting, $.reference_setting, $.value_line),

    // osc saw voices=3 spread=7
    setting: $ => seq(
      field('name', alias($.identifier, $.setting_name)),
      repeat($._item),
    ),

    // instrument lead | sidechain drums depth=0.8 | layer drums | source lead bars=1-2
    reference_setting: $ => seq(
      field('name', alias(choice('instrument', 'sidechain', 'layer', 'source'), $.setting_name)),
      after($, field('target', alias($.identifier, $.reference))),
    ),

    // A body line that does not start with a word.
    value_line: $ => seq($._plain_item, repeat($._item)),

    // ------------------------------------------------------------ tracks

    _track_line: $ => choice($._setting_line, $.play_step, $.sweep),

    // play verse x2 transpose=2 vel=0.9 | play bars=17-24 x2 | play all
    play_step: $ => seq(
      'play',
      after($, field('pattern', alias($.identifier, $.reference))),
    ),

    // sweep cutoff from=0.2 to=0.7 bars=9-16
    sweep: $ => seq(
      field('name', alias('sweep', $.setting_name)),
      after($, field('parameter', alias($.identifier, $.parameter))),
    ),

    // ------------------------------------------------------------ patterns

    _pattern_line: $ => choice($.grid_row, $.note_line),

    // kick X.....x.X.....x.
    grid_row: $ => seq(
      field('voice', choice($.note, alias($.identifier, $.drum))),
      $._grid_cell,
      repeat(choice($._grid_cell, $.bar, $._junk)),
    ),

    _grid_cell: $ => choice($.grid_hit, $.grid_hold, $.grid_rest),

    // A4:q A4:e [D3 A3 F4]:w D4:h@80 r |
    //
    // A line that starts with a bare note or drum is followed by something
    // other than a word: that keeps `kick x-x-` a grid row (a word would be
    // the longer match for `x-x-`). A word there parses as a stray `word`.
    note_line: $ => choice(
      seq(
        alias($._bare_event, $.event),
        optional(seq(
          choice(
            alias($._note_event, $.event),
            alias($._other_event, $.event),
            $.bar,
            $._junk,
          ),
          repeat($._note_item),
        )),
      ),
      seq(
        choice(
          alias($._suffixed_event, $.event),
          alias($._other_event, $.event),
          $.bar,
          $._junk,
        ),
        repeat($._note_item),
      ),
    ),

    _note_item: $ => choice(
      alias($._note_event, $.event),
      alias($._drum_event, $.event),
      alias($._other_event, $.event),
      $.bar,
      $._junk,
    ),

    // kick | A4
    _bare_event: $ => field('pitch', choice($.note, alias($.identifier, $.drum))),

    // kick:q | A4:q@80!
    _suffixed_event: $ => seq(
      field('pitch', choice($.note, alias($.identifier, $.drum))),
      suffix($),
    ),

    _note_event: $ => prec.right(seq(field('pitch', $.note), optional(suffix($)))),

    _drum_event: $ => prec.right(seq(field('pitch', alias($.identifier, $.drum)), optional(suffix($)))),

    // r:q | [C4 E4 G4]:h
    _other_event: $ => prec.right(seq(field('pitch', choice($.rest, $.chord)), optional(suffix($)))),

    chord: $ => prec.right(seq(
      '[',
      repeat(choice($.note, $.rest, alias($.identifier, $.drum), $._junk)),
      optional(']'),
    )),

    rest: _ => choice('r', '_'),

    bar: _ => '|',

    // ------------------------------------------------------------ items

    _item: $ => choice($.identifier, $._plain_item),

    // Every item that does not start with a bare word.
    _plain_item: $ => choice(
      $.option,
      $.string,
      $.number,
      $.fraction,
      $.range,
      $.note,
      $.note_range,
      $.repeat_count,
      $.bar,
      $._junk,
    ),

    // key=value
    option: $ => prec.right(seq(
      field('key', alias($.identifier, $.key)),
      '=',
      optional(field('value', choice(
        $.identifier,
        $.string,
        $.number,
        $.fraction,
        $.range,
        $.note,
        $.note_range,
        $._junk,
      ))),
    )),


    // ------------------------------------------------------------ tokens
    //
    // Order matters: when two tokens match the same text, the one defined
    // first wins (a duration `q` over a word `q`, a grid hit `x` over a word
    // `x`, a note `A4` over a word `A4`).

    comment: _ => token(seq('#', /[^\n]*/)),

    string: _ => token(choice(/"[^"\n]*"/, /"[^"\n]*/)),

    // w h q e s t, dotted q., triplet e3, fraction 3/16, sum h+e
    duration: _ => token(/([0-9]+\/[0-9]+|[whqest]3?)\.*(\+([0-9]+\/[0-9]+|[whqest]3?)\.*)*/),

    // accent ! and slide ~
    articulation: _ => token(/[!~]+/),

    // Grid cells: x X o 1-9 hit, = hold, . - _ rest.
    grid_hit: _ => token(/[xXo1-9]+/),
    grid_hold: _ => token(/=+/),
    grid_rest: _ => token(/[._-]+/),

    // x2 after `play`
    repeat_count: _ => token(/x[0-9]+/),

    // C1-B3
    note_range: _ => token(new RegExp(NOTE + '-' + NOTE)),

    // C4 F#3 Bb2 C-1
    note: _ => token(new RegExp(NOTE)),

    // 17-32, 0.5s-1.7s, 1-127
    range: _ => token(new RegExp(NUM + '(ms|s|k)?-' + NUM + '(ms|s|k)?')),

    // 4/4, 1/16
    fraction: _ => token(/[0-9]+\/[0-9]+/),

    // 120, -3, +4, 0.58, 10ms, 1.2k, -3dB, 440Hz
    number: _ => token(new RegExp(NUM + '(ms|s|dB|db|kHz|khz|Hz|hz|k|%)?')),

    identifier: _ => /[A-Za-z0-9][A-Za-z0-9_-]*/,

    // Anything the lexer cannot place otherwise; never an error.
    _junk: $ => alias($._junk_token, $.word),
    _junk_token: _ => token(prec(-1, /[^\s#]+/)),

    _line_break: _ => token(/\r?\n([ \t\r\f]*\n)*/),
    _indent: _ => token(/\r?\n([ \t\r\f]*\n)*[ \t\f]+/),
  },
});

/** What follows a pitch: `:q`, `@80`, `!`, in that order, at least one. */
function suffix($) {
  const duration = seq(':', optional(field('duration', $.duration)));
  const velocity = seq('@', optional(field('velocity', alias($.number, $.velocity))));
  const articulation = field('articulation', $.articulation);
  return choice(
    seq(duration, optional(velocity), optional(articulation)),
    seq(velocity, optional(articulation)),
    articulation,
  );
}

/**
 * The indented lines of a block. A comment at column 0 between them does not
 * end the block: that needs two tokens of lookahead (a line break, then an
 * indent), which the `conflicts` entries let the parser decide.
 */
function body($, line) {
  return repeat(choice(
    seq($._indent, optional(line)),
    prec.dynamic(1, seq(repeat1($._line_break), $._indent, optional(line))),
  ));
}

/**
 * A slot for a word (a name or a reference) followed by anything; a line whose
 * first item is not a word still parses.
 */
function after($, slot) {
  return optional(choice(
    seq(slot, repeat($._item)),
    seq($._plain_item, repeat($._item)),
  ));
}
