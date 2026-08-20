/**
 * 项目级原生提示词文件 barrel。
 */

export {
  PROJECT_INSTRUCTION_FILES,
  filesForAgent,
  matchProjectInstructionNodeName,
  resolveActiveFileId,
  shouldGuardProjectInstructionContextChange,
} from './projectInstructionFiles';
export type {
  ProjectInstructionFileId,
  ProjectInstructionFileSpec,
  ProjectInstructionGuardInput,
} from './projectInstructionFiles';

export {
  useProjectInstructionFilesController,
} from './useProjectInstructionFilesController';
export type {
  ProjectInstructionBusyAction,
  ProjectInstructionFileState,
  UseProjectInstructionFilesControllerArgs,
  UseProjectInstructionFilesControllerResult,
} from './useProjectInstructionFilesController';

export { ProjectInstructionFilesView } from './ProjectInstructionFilesView';
export type {
  ProjectInstructionFilesViewLabels,
  ProjectInstructionFilesViewProps,
} from './ProjectInstructionFilesView';
