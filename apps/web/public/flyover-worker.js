// Module worker for the browser viewer. Fetches one tile's geometry and layer tiles from the API,
// decodes and extrudes them with the wasm renderer's `prepare_tile`, and transfers the finished
// vertex and feature bytes back to the page. A `text` message does the same for one file's `.ftx`
// source, laying it out on the roof rectangle the page sends. Keeps fetch, zstd decode, mesh
// building, and text layout off the page's main thread (SPEC 2.6, 2.4).

import init, { prepare_text, prepare_tile } from "/renderer/flyover_web.js";

const ready = init();
let config = null;

self.onmessage = async (event) => {
  const msg = event.data;
  if (msg.type === "config") {
    config = msg.config;
    return;
  }
  if (msg.type === "text") {
    const { id, rect, z, firstLine, base } = msg;
    try {
      await ready;
      const url = `${base}/text/${id >>> 12}/${id}.ftx`;
      const res = await fetch(url);
      if (!res.ok) throw new Error(`${res.status} fetching ${url}`);
      const ftx = new Uint8Array(await res.arrayBuffer());
      const [instances, meta] = prepare_text(id, ftx, new Float32Array(rect), z, firstLine);
      self.postMessage({ type: "text", id, instances, meta }, [instances.buffer, meta.buffer]);
    } catch (err) {
      self.postMessage({ type: "textError", id, message: String(err) });
    }
    return;
  }
  if (msg.type !== "tile") return;

  const { z, x, y, base } = msg;
  try {
    await ready;
    const urls = [
      `${base}/tiles/${z}/${x}/${y}.fly`,
      `${base}/layers/${config.heightLayer}/${z}/${x}/${y}.flv`,
      `${base}/layers/${config.colorLayer}/${z}/${x}/${y}.flv`,
    ];
    const [fly, height, color] = await Promise.all(
      urls.map(async (url) => {
        const res = await fetch(url);
        if (!res.ok) throw new Error(`${res.status} fetching ${url}`);
        return new Uint8Array(await res.arrayBuffer());
      }),
    );
    const [vertices, features, roofs] = prepare_tile(
      z,
      x,
      y,
      fly,
      height,
      color,
      new Float64Array(config.bounds),
      new Float32Array(config.palette),
      config.heightScale,
    );
    self.postMessage({ type: "tile", z, x, y, vertices, features, roofs }, [
      vertices.buffer,
      features.buffer,
      roofs.buffer,
    ]);
  } catch (err) {
    self.postMessage({ type: "error", z, x, y, message: String(err) });
  }
};
