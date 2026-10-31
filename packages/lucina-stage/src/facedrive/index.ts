/** Face tracking drive (F1): the shared solver for ARKit / MediaPipe output and the stage-side driver. */
export { FACE_CHANNELS, FACE_LIMITS, FaceDriver, NECK_SHARE } from './drive';
export type { FaceChannel, FaceDrive } from './drive';
export { FaceSolver, HeadPoseSolver, faceDriveFromBlendshapes, ramp } from './solver';
export type { Blendshapes, HeadPose, SolveOptions } from './solver';
