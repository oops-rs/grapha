import { formatLabel, ParentWorker } from "./models";

/** Coordinates the TypeScript fixture. */
export class TypeScriptWorker extends ParentWorker {
  public onReady = () => reportReady();
  private label = formatLabel("typescript");

  public run(): void {
    this.onReady();
  }
}

export enum TypeScriptStatus {
  Ready,
  Stopped,
}

export function reportReady(): void {}

const worker = new TypeScriptWorker();
worker.run();
