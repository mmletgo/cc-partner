/**
 * Agent Hub 提示词三栏 barrel。
 */

export {
  addBlock,
  initialThreePaneFromDisk,
  parseBlocksFromOriginal,
  recomputePreview,
  resolveSyncContent,
  updateBlock,
  updateOriginalText,
} from './instructionThreePane';
export type {
  InstructionBlockDraft,
  InstructionThreePaneState,
  SyncBaseline,
} from './instructionThreePane';

export {
  InstructionThreePaneView,
} from './InstructionThreePaneView';
export type {
  InstructionThreePaneViewLabels,
  InstructionThreePaneViewProps,
} from './InstructionThreePaneView';

export {
  originalFromWorkspace,
  useInstructionThreePaneController,
} from './useInstructionThreePaneController';
export type {
  UseInstructionThreePaneControllerArgs,
  UseInstructionThreePaneControllerResult,
} from './useInstructionThreePaneController';
