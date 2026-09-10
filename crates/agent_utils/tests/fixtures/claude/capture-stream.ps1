[Console]::Out.WriteLine('{"type":"system","subtype":"init","session_id":"capture","model":"fake-claude","permissionMode":"default"}')

$capture = [System.IO.StreamWriter]::new($env:NMT_FAKE_STREAM_LOG, $false, [System.Text.UTF8Encoding]::new($false))

try {
    while ($null -ne ($line = [Console]::In.ReadLine())) {
        $capture.WriteLine($line)
        $capture.Flush()
    }
} finally {
    $capture.Dispose()
}
