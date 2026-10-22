export { createLucinaStage } from './stage';
export { gltfExtensions, loadModel } from './loader';
export type { LoadedModel, LoadOptions } from './loader';
export type { LucinaStage, LucinaStageOptions, SpeakOptions, StageInfo, TapEvent, TouchEvent } from './stage';
export * from './interact';
export {
  createVoice,
  getSpeechRecognition,
  isListenSupported,
  isVoiceError,
  listen,
  VoiceError,
} from './voice';
export type { ListenOptions, Voice, VoiceErrorCode, VoiceOptions } from './voice';
export { MUTE_STORAGE_KEY, readMuted, readStoredMuted, writeMuted } from './mute';
export { FrameStats } from './stats';
export type { FrameListener, FrameSample } from './stats';
export { normalizeManifest, resolveBones, resolveMorphs } from './manifest';
export type { NormalizedManifest, CameraFraming } from './manifest';
export { synthSpeechWav } from './synth';
export { ClipPlayer, clipLibraryFrom, loadClipLibrary, DEFAULT_STATE_CLIPS } from './clips';
export type { ClipInfo, ClipLibrary, PlayOptions } from './clips';
export type { RigReport } from './rig';
export * from './types';
export { isStageQuality, registerClip, STAGE_QUALITIES } from './scene';
export type { Clip, ClipSample, EnvironmentOptions, StageQuality, StageQualitySetting, StageTheme } from './scene';
export { createWardrobeStage, outfitName, outfitUrls, parseWardrobe, parseWardrobeFile, subjectRect } from './wardrobe';
export { HairRig, NATURAL_HAIR, parseHairColors, parseHairstyles, tintMaterial } from './hair';
export type { HairColor, HairEntry } from './hair';
export type { SetModelOptions, StageFactory, WardrobeEntry, WardrobeStage, WardrobeStageOptions } from './wardrobe';
export { EFFECT_DURATION_MS, effectPhases, playOutfitEffect } from './outfitEffect';
export type { OutfitEffect, OutfitEffectOptions, SubjectRect } from './outfitEffect';

export { applyToon, normalizeToon, type ToonConfig, type ToonMaterialConfig, type ToonHandle, type ToonBloom, type ToonOptions, type ToonSource } from './toon';
