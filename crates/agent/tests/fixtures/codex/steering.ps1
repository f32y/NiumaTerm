$ErrorActionPreference = 'Stop'
$turnNumber = 0

function Write-Message($message) {
    [Console]::Out.WriteLine(($message | ConvertTo-Json -Depth 30 -Compress))
}

while ($null -ne ($line = [Console]::In.ReadLine())) {
    [System.IO.File]::AppendAllText($env:NMT_FAKE_REQUEST_LOG, "$line`n")
    $request = $line | ConvertFrom-Json
    if ($null -eq $request.id) { continue }

    switch ($request.method) {
        'thread/start' {
            Write-Message @{ id = $request.id; result = @{ thread = @{ id = 'thread-test' } } }
        }
        'turn/start' {
            $turnNumber++
            $turn = @{ id = "turn-$turnNumber"; status = 'inProgress' }
            Write-Message @{ id = $request.id; result = @{ turn = $turn } }
            Write-Message @{ method = 'turn/started'; params = @{ threadId = 'thread-test'; turn = $turn } }
            Write-Message @{ method = 'item/started'; params = @{
                threadId = 'thread-test'; turnId = $turn.id
                item = @{ type = 'userMessage'; id = "user-$turnNumber"; content = $request.params.input }
            } }
        }
        'turn/steer' {
            $completed = @{ method = 'turn/completed'; params = @{
                threadId = 'thread-test'; turn = @{ id = "turn-$turnNumber"; status = 'completed' }
            } }
            if ($env:NMT_FAKE_COMPLETE_FIRST -eq 'true') { Write-Message $completed }
            Write-Message @{ id = $request.id; error = @{ code = -32600; message = 'no active turn to steer' } }
            if ($env:NMT_FAKE_COMPLETE_FIRST -ne 'true') { Write-Message $completed }
        }
        default { Write-Message @{ id = $request.id; result = @{} } }
    }
}
