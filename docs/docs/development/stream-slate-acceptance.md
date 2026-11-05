# Stream slate acceptance plan

This plan gives the manual tests for the stream slates. Each test examines one transition from a slate to live media on one client.

Unit tests examine the packet rules of the splice. These manual tests examine the behavior of real players. Do these tests before each release that changes the stream path.

## Slates and transitions

The relay has three slates:

| Slate | Message | Condition |
|---|---|---|
| Starting | Stream starting in N seconds | The live buffer of a new session fills. |
| Busy | All tuners in use, stay tuned for one to open up | All provider connections of the pool are in use. |
| Retry | Having trouble reaching this channel, retrying | The provider or the input adapter fails. |

Examine these transitions:

| ID | Transition |
|---|---|
| T1 | Starting slate to live media |
| T2 | Busy slate to live media |
| T3 | Retry slate to live media |
| T4 | Busy slate to retry slate, then to live media |
| T5 | Live media with no slate (a channel that another viewer watches) |

## Clients

Do each transition on each client:

| ID | Client |
|---|---|
| C1 | Roku with the Jellyfin app |
| C2 | Jellyfin for Android TV |
| C3 | Moonfin for Android |
| C4 | Jellyfin web client in a desktop browser |
| C5 | Jellyfin HLS transcoder (a client that forces transcode) |

For C5, set a low maximum bitrate in the client. This setting makes Jellyfin transcode the stream to HLS.

## Preparation

1. Use a test instance of the relay. Do not use the production provider connections of other people.
2. Set the connection limit of the test provider pool to a known value, for example 1.
3. Add a test source that you control. For example, use a local HTTP server that serves an MPEG-TS file.
4. Make sure that the test source can stop and start while a client watches.
5. Make sure that the passive capture measured each test channel. The stream health page shows the resolution and the codecs.
6. Open the relay log in a terminal.
7. Write the client name, the client version, and the relay version in the result table.

Do not put tokens or provider URLs in the result table. Write `/out/<token>/` in each URL.

## Procedures

### T1: Starting slate to live media

1. Make sure that no viewer watches the test channel.
2. Make sure that `media.viewer.start_delay_seconds` has its default value.
3. Start the test channel on the client.
4. Look for the starting slate. The countdown must decrease each second.
5. Wait for live media.
6. Record the time from the first slate frame to the first live frame.

### T2: Busy slate to live media

1. Start channels on other clients until all provider connections of the pool are in use.
2. Start the test channel on the client under test.
3. Look for the busy slate.
4. Wait 30 seconds.
5. Make sure that the client shows the busy slate for all 30 seconds.
6. Stop one of the other channels.
7. Wait for live media on the client under test.
8. Record the time from the stop to the first live frame.

### T3: Retry slate to live media

1. Stop the test source.
2. Start the test channel on the client.
3. Look for the retry slate.
4. Wait 60 seconds.
5. Make sure that the relay log shows retries at 1, 2, 4, 8, 16, and 30 second intervals.
6. Start the test source.
7. Wait for live media.
8. Record the time from the source start to the first live frame.

### T4: Busy slate to retry slate, then to live media

1. Do steps 1 to 3 of T2.
2. Stop the test source.
3. Stop one of the other channels.
4. Look for the retry slate.
5. Start the test source.
6. Wait for live media.

### T5: Live media with no slate

1. Start the test channel on a different client.
2. Wait for live media on that client.
3. Start the same channel on the client under test.
4. Make sure that live media starts with no slate.

## Pass criteria

A test passes only when all of these conditions are true:

1. The client shows each expected slate. The text is legible and centered.
2. The client does not show an error message.
3. The client does not stop or restart playback at a transition.
4. Live media starts within 10 seconds after the condition clears. For T3, add the current retry interval.
5. The picture does not freeze for more than 2 seconds after a transition.
6. Audio plays after each transition. The number of audio channels does not change.
7. After 60 seconds of live media, audio and video are in sync. The offset must be less than 0.5 seconds.
8. Seek and pause controls operate after a transition, if the client has them.
9. The relay log shows no more open provider connections than the pool limit.
10. For C5, the Jellyfin log shows no FFmpeg error at a transition. The transcode continues and does not restart.

If a test fails, record the relay log lines for the test period. Remove tokens and provider URLs from the log lines before you attach them.

## Result table

Copy this table for each release. Write `Pass`, `Fail`, or `Not done` in each cell. Add a note for each failure.

| Transition | C1 Roku | C2 Android TV | C3 Moonfin | C4 Web | C5 HLS transcoder |
|---|---|---|---|---|---|
| T1 Starting to live | | | | | |
| T2 Busy to live | | | | | |
| T3 Retry to live | | | | | |
| T4 Busy to retry to live | | | | | |
| T5 Live with no slate | | | | | |
