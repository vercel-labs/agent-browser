/** Draw a decoded frame only while its effect is current, then release its bitmap. */
export function drawStreamFrame(
  canvas: HTMLCanvasElement,
  base64: string,
  onDraw?: () => void,
): () => void {
  let current = true;
  void (async () => {
    let bitmap: ImageBitmap | undefined;
    try {
      const binary = atob(base64);
      const bytes = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
      bitmap = await createImageBitmap(new Blob([bytes], { type: "image/jpeg" }));
      if (!current) return;
      const context = canvas.getContext("2d");
      if (!context) return;
      canvas.width = bitmap.width;
      canvas.height = bitmap.height;
      context.drawImage(bitmap, 0, 0);
      onDraw?.();
    } catch {
      // A damaged frame can be skipped; the next frame can still be displayed.
    } finally {
      bitmap?.close();
    }
  })();
  return () => { current = false; };
}
