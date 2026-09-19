<?php

namespace App;

use App\Models\User;
use App\Support\Str;

class Service
{
    public function handle(): string
    {
        return "ok";
    }
}

interface Handler
{
    public function run(): void;
}

function helper(): int
{
    return 1;
}
