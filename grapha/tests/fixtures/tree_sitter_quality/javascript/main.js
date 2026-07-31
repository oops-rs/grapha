import { formatLabel } from "./helpers.js";

/** Coordinates the JavaScript fixture. */
export class JavaScriptWorker {
  onReady = () => reportReady();
  label = formatLabel("javascript");

  run() {
    this.onReady();
  }
}

export function reportReady() {}

const worker = new JavaScriptWorker();
worker.run();
