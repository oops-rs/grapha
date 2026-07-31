<?php
declare(strict_types=1);

namespace Quality;

use Quality\Support\Formatter;

/** Coordinates the PHP fixture. */
final class PhpWorker extends BaseWorker implements Runnable
{
    private \Closure $onReady;
    private string $label;

    public function __construct()
    {
        $this->onReady = fn () => $this->reportReady();
        $this->label = Formatter::format("php");
    }

    public function run(): void
    {
        ($this->onReady)();
    }

    private function reportReady(): void {}
}

interface Runnable
{
    public function run(): void;
}

abstract class BaseWorker {}

enum PhpStatus: string
{
    case Ready = "ready";
    case Stopped = "stopped";
}

(new PhpWorker())->run();
