import { renderBadge } from "./widgets";

/** Coordinates the TSX fixture. */
export class TsxWorker {
  public onReady = () => reportReady();
  private label = renderBadge("tsx");

  public render() {
    return <span>{this.label}</span>;
  }
}

export function reportReady(): void {}

const worker = new TsxWorker();
worker.render();
