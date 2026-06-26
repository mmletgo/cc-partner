import type { Extension } from '@codemirror/state';
import { HighlightStyle, syntaxHighlighting } from '@codemirror/language';
import { EditorView } from '@codemirror/view';
import { tags } from '@lezer/highlight';

export const WORKBENCH_ONE_DARK_PRO_COLORS = {
  cursor: '#528bff',
  foreground: '#abb2bf',
  muted: '#7d8799',
  keyword: '#c678dd',
  property: '#e06c75',
  function: '#61afef',
  type: '#e5c07b',
  string: '#98c379',
  number: '#d19a66',
  operator: '#56b6c2',
  invalid: '#ffffff',
} as const;

export const WORKBENCH_CODE_EDITOR_THEME: Extension = EditorView.theme(
  {
    '&': {
      color: 'var(--fg)',
      backgroundColor: 'var(--surface)',
    },
    '.cm-content': {
      caretColor: WORKBENCH_ONE_DARK_PRO_COLORS.cursor,
    },
    '.cm-cursor, .cm-dropCursor': {
      borderLeftColor: WORKBENCH_ONE_DARK_PRO_COLORS.cursor,
    },
    '&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection': {
      backgroundColor: 'var(--accent-soft)',
    },
    '.cm-panels': {
      backgroundColor: 'var(--surface-warm)',
      color: 'var(--fg)',
    },
    '.cm-searchMatch': {
      backgroundColor: 'var(--accent-soft)',
      outline: '1px solid var(--accent)',
    },
    '.cm-searchMatch.cm-searchMatch-selected': {
      backgroundColor: 'color-mix(in oklab, var(--accent) 22%, transparent)',
    },
    '.cm-activeLine': {
      backgroundColor: 'var(--accent-soft)',
    },
    '.cm-selectionMatch': {
      backgroundColor: 'color-mix(in oklab, var(--success) 14%, transparent)',
    },
    '&.cm-focused .cm-matchingBracket, &.cm-focused .cm-nonmatchingBracket': {
      backgroundColor: 'color-mix(in oklab, var(--accent) 22%, transparent)',
    },
    '.cm-gutters': {
      backgroundColor: 'var(--bg)',
      color: 'var(--meta)',
      border: 'none',
    },
    '.cm-activeLineGutter': {
      backgroundColor: 'var(--surface-warm)',
    },
    '.cm-foldPlaceholder': {
      backgroundColor: 'transparent',
      border: 'none',
      color: 'var(--fg)',
    },
    '.cm-tooltip': {
      border: 'none',
      backgroundColor: 'var(--surface-warm)',
      color: 'var(--fg)',
    },
    '.cm-tooltip .cm-tooltip-arrow:before': {
      borderTopColor: 'transparent',
      borderBottomColor: 'transparent',
    },
    '.cm-tooltip .cm-tooltip-arrow:after': {
      borderTopColor: 'var(--surface-warm)',
      borderBottomColor: 'var(--surface-warm)',
    },
    '.cm-tooltip-autocomplete > ul > li[aria-selected]': {
      backgroundColor: 'var(--accent-soft)',
      color: 'var(--fg)',
    },
  }
);

const WORKBENCH_ONE_DARK_PRO_HIGHLIGHT = HighlightStyle.define([
  { tag: tags.keyword, color: WORKBENCH_ONE_DARK_PRO_COLORS.keyword },
  {
    tag: [tags.name, tags.deleted, tags.character, tags.propertyName, tags.macroName],
    color: WORKBENCH_ONE_DARK_PRO_COLORS.property,
  },
  {
    tag: [tags.function(tags.variableName), tags.labelName],
    color: WORKBENCH_ONE_DARK_PRO_COLORS.function,
  },
  {
    tag: [tags.color, tags.constant(tags.name), tags.standard(tags.name)],
    color: WORKBENCH_ONE_DARK_PRO_COLORS.number,
  },
  {
    tag: [tags.definition(tags.name), tags.separator],
    color: WORKBENCH_ONE_DARK_PRO_COLORS.foreground,
  },
  {
    tag: [
      tags.typeName,
      tags.className,
      tags.number,
      tags.changed,
      tags.annotation,
      tags.modifier,
      tags.self,
      tags.namespace,
    ],
    color: WORKBENCH_ONE_DARK_PRO_COLORS.type,
  },
  {
    tag: [
      tags.operator,
      tags.operatorKeyword,
      tags.url,
      tags.escape,
      tags.regexp,
      tags.link,
      tags.special(tags.string),
    ],
    color: WORKBENCH_ONE_DARK_PRO_COLORS.operator,
  },
  { tag: [tags.meta, tags.comment], color: WORKBENCH_ONE_DARK_PRO_COLORS.muted },
  { tag: tags.strong, fontWeight: 'bold' },
  { tag: tags.emphasis, fontStyle: 'italic' },
  { tag: tags.strikethrough, textDecoration: 'line-through' },
  { tag: tags.link, color: WORKBENCH_ONE_DARK_PRO_COLORS.muted, textDecoration: 'underline' },
  { tag: tags.heading, fontWeight: 'bold', color: WORKBENCH_ONE_DARK_PRO_COLORS.property },
  { tag: [tags.atom, tags.bool, tags.special(tags.variableName)], color: WORKBENCH_ONE_DARK_PRO_COLORS.number },
  {
    tag: [tags.processingInstruction, tags.string, tags.inserted],
    color: WORKBENCH_ONE_DARK_PRO_COLORS.string,
  },
  { tag: tags.invalid, color: WORKBENCH_ONE_DARK_PRO_COLORS.invalid },
]);

export const WORKBENCH_ONE_DARK_PRO_SYNTAX_EXTENSION: Extension = [
  syntaxHighlighting(WORKBENCH_ONE_DARK_PRO_HIGHLIGHT),
];
