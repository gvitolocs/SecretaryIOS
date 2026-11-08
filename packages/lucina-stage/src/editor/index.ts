/** The character editor ("Edit Lucina"): docs/design/custom-cast-editor.md. */
export * from './look';
export { BodyDeformer, bodyDeformerFor, bodyFactors, installSkinDeform, skinDeformOf, BODY_BONES } from './body';
export { FaceMorpher, faceMorpherFor, faceFields, faceLandmarks, findFaceParts, FACE_MATERIALS, MORPH_PREFIX } from './face';
export type { FaceLandmarks, FacePart, Field } from './face';
export { Painter, makeupAt, makeupMask, patchToon, SKIN_MATERIALS, NATURAL_SKIN } from './paint';
export { AccessoryRig, accessoryName, parseAccessories, rebindSkinned, SLOT_BONES } from './accessories';
export type { AccessoryEntry } from './accessories';
export { BASE_OUTFIT, parseParts, PartsRig, resolveParts } from './parts';
export type { PartEntry } from './parts';
export { applyLook, applyLookSync, createAvatarEditor, editorPrepareModel, editorStateFor } from './editor';
export type { AvatarEditor, EditorCatalog, EditorContext, EditorOptions } from './editor';
