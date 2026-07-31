<?php
declare(strict_types=1);

namespace Quality\Support;

final class Formatter
{
    public static function format(string $value): string
    {
        return trim($value);
    }
}
