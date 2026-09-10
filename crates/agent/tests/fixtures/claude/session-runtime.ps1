[Console]::Out.WriteLine('{"type":"system","subtype":"init","session_id":"40000000-0000-4000-8000-000000000000","model":"fake-claude","permissionMode":"default"}')
while ($null -ne ($messageLine = [Console]::In.ReadLine())) {
    [System.IO.File]::AppendAllText($env:NMT_FAKE_STREAM_LOG, $messageLine + [Environment]::NewLine)
}
