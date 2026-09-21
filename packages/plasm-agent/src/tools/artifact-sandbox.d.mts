export const IMAGE_PIN_RE: RegExp;
export const MAX_INPUT_BYTES: number;
export const MAX_RESULT_BYTES: number;
export type TransformRequest = { code: string; artifacts: unknown[] };
export function validateTransformRequest(value: unknown): TransformRequest;
export function runSandboxTransform(request: TransformRequest, image: string): Promise<string>;
