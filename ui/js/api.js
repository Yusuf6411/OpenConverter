// Thin client for the local OpenConverter API. Every request carries
// X-OpenConverter: 1 (required by the server's CSRF check).

export class ApiError extends Error {
  constructor(status, code, message) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

async function parse(resp) {
  let body = null;
  const type = resp.headers.get("Content-Type") || "";
  if (type.startsWith("application/json")) {
    try { body = await resp.json(); } catch (e) { body = null; }
  }
  if (!resp.ok) {
    const err = body && body.error ? body.error : { code: "http_" + resp.status, message: "Request failed (" + resp.status + ")." };
    throw new ApiError(resp.status, err.code, err.message);
  }
  return body;
}

export async function request(method, path, data) {
  const opts = { method, headers: { "X-OpenConverter": "1" }, credentials: "same-origin" };
  if (data !== undefined) {
    opts.headers["Content-Type"] = "application/json";
    opts.body = JSON.stringify(data);
  }
  let resp;
  try {
    resp = await fetch(path, opts);
  } catch (e) {
    throw new ApiError(0, "network", "Cannot reach the OpenConverter server. Is it still running?");
  }
  return parse(resp);
}

export const api = {
  health: () => request("GET", "/api/health"),
  formats: () => request("GET", "/api/formats"),
  settings: () => request("GET", "/api/settings"),
  saveSettings: (s) => request("PUT", "/api/settings", s),
  file: (id) => request("GET", "/api/files/" + id),
  lookup: (ids, outputs) => request("GET", "/api/files?ids=" + ids.map(encodeURIComponent).join(",") +
    "&outputs=" + outputs.map(encodeURIComponent).join(",")),
  createJob: (fileId, target, options) => request("POST", "/api/jobs", { file_id: fileId, target_format: target, options }),
  archiveEntries: (fileId, password) => request("POST", "/api/files/" + fileId + "/entries", password ? { password } : {}),
  archiveExtract: (fileId, paths, password) => request("POST", "/api/files/" + fileId + "/extract", { paths, ...(password ? { password } : {}) }),
  createArchive: (body) => request("POST", "/api/archives", body),
  job: (id) => request("GET", "/api/jobs/" + id),
  jobs: () => request("GET", "/api/jobs"),
  cancel: (id) => request("POST", "/api/jobs/" + id + "/cancel"),
  history: () => request("GET", "/api/history"),
  clearHistory: () => request("DELETE", "/api/history"),
  deleteHistory: (id) => request("DELETE", "/api/history/" + id),
  downloadUrl: (outputId) => "/api/outputs/" + outputId + "/download",
  outputPreviewUrl: (outputId) => "/api/outputs/" + outputId + "/preview",
  filePreviewUrl: (fileId) => "/api/files/" + fileId + "/preview",
  eventsUrl: (jobId) => "/api/jobs/" + jobId + "/events",
};

// Upload one file with real upload progress (XMLHttpRequest exposes it; fetch does not).
export function uploadFile(file, onProgress) {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", "/api/files");
    xhr.setRequestHeader("X-OpenConverter", "1");
    xhr.responseType = "json";
    xhr.upload.addEventListener("progress", (e) => {
      if (e.lengthComputable && onProgress) onProgress(e.loaded / e.total);
    });
    xhr.addEventListener("load", () => {
      const body = xhr.response;
      if (xhr.status >= 200 && xhr.status < 300 && body && body.files) {
        resolve(body.files[0]);
      } else {
        const err = body && body.error ? body.error : { code: "http_" + xhr.status, message: "Upload failed (" + xhr.status + ")." };
        reject(new ApiError(xhr.status, err.code, err.message));
      }
    });
    xhr.addEventListener("error", () => reject(new ApiError(0, "network", "The upload was interrupted. Is the server still running?")));
    const form = new FormData();
    form.append("files", file, file.name);
    xhr.send(form);
  });
}
