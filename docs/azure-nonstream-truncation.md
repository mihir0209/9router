Azure non-streaming truncation — root cause

Reproduced 2026-09-30 against Cloudflare worker f323aa6a.

Direct to Azure (southindia, gpt-5.6-terra, max_completion_tokens):
  5/5 http=200, ~1533 bytes, JSON.parse VALID, streaming has exactly 1 [DONE].
  Azure is not at fault.

Through 9router (same model, client sends stream:false):
  5/5 http=200, ~1545 bytes, JSON.parse THROWS — body ends mid-object:
    ..."rejected_prediction_tokens":0}}}

The response header is the tell:
  content-type: text/event-stream

for a request that asked for no streaming. The body is a bare JSON object, not
SSE-framed, but it is delivered over the streaming response path and the stream
closes before the object is complete. Hence ~12 bytes more than Azure's own
response and invalid.

So the stream is being selected on the provider's force-stream preference rather
than on the client's `stream` flag, and the non-streaming client is served through
the SSE writer. Anything that frames a JSON body as event-stream risks the body
being cut at close.

Not yet located: the exact branch that picks the streaming handler for a
non-streaming request. `handleNonStreamingResponse` parses properly and would
emit valid JSON, so the request is not reaching it.
