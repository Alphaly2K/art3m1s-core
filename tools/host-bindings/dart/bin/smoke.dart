import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:typed_data';

import 'package:artemis_host_v2/artemis_file_io.dart';
import 'package:artemis_host_v2/artemis_host.dart';

void require(bool condition, String message) {
  if (!condition) throw StateError(message);
}

void exerciseHost(
  ArtemisSession session,
  ArtemisSession other, {
  required bool mediaEnabled,
}) {
  const ini = '[WINDOWS]\nWIDTH=320\nHEIGHT=200\nBOOT=boot.iet\n';
  final boot = utf8.encode(
    '*top\n[lyc id=1 width=2 height=1 color=ff112233]\n'
    '[setonwindowbutton button=1 label=window call=1]\n'
    '[exec command=fullscreen]\n[caption data=${'x' * 5000}]\n'
    'original text\n[wt0]\n[caption data=advanced]\n'
    '[seplay id=fx file=ping.ogg gain=450]\n[wait se=fx]\n'
    '[lyc id=1.80]\n[video id=1.80 file=overlay.ogv]\n'
    '[wait video=1.80]\n[caption data=media_done]\n[stop]\n'
    '*window\n[caption data=window]\n[return]\n',
  );
  session.openProject(utf8.encode(ini), platform: 'windows');

  void send(ArtemisMessage message) {
    require(session.submit([message]), 'The smoke batch should fit');
  }

  final epoch = session.state.epoch;
  final effects = <String>[];
  ArtemisEvent? audioEvent, videoEvent;
  var checkedReplyOwner = false;
  void pump(int frame) {
    for (var turn = 0; turn < 64; turn++) {
      final result = session.step(
        frameId: frame,
        monotonicNs: frame * 1000000,
        utcUnixMs: 0,
        utcOffsetMinutes: 0,
      );
      for (
        var events = session.pollEvents();
        events.isNotEmpty;
        events = session.pollEvents()
      ) {
        for (final event in events) {
          final metadata = event.metadata;
          switch ((event.domain, event.kind)) {
            case (ART_DOMAIN_IO, ART_IO_STAT):
              final found = metadata['key'] == 'boot.iet';
              final reply = event.reply({
                'status': 'ok',
                'found': found,
                if (found) 'size': '${boot.length}',
                if (found) 'revision': '1',
              });
              if (!checkedReplyOwner) {
                try {
                  other.submit([reply]);
                  throw StateError('A different session accepted the reply');
                } on ArgumentError {
                  checkedReplyOwner = true;
                }
              }
              send(reply);
            case (ART_DOMAIN_IO, ART_IO_READ):
              final offset = int.parse(metadata['offset'] as String);
              final length = int.parse(metadata['length'] as String);
              require(
                metadata['key'] == 'boot.iet',
                'Unexpected read: $metadata',
              );
              final end = (offset + length).clamp(offset, boot.length);
              send(
                event.reply({
                  'status': 'ok',
                  'revision': '1',
                  'eof': end == boot.length,
                }, blob: boot.sublist(offset, end)),
              );
            case (ART_DOMAIN_UI, ART_UI_CAPTION):
              final caption = metadata['text'] as String;
              effects.add(caption == 'x' * 5000 ? 'large' : caption);
            case (ART_DOMAIN_UI, ART_UI_WINDOW_ACTION):
              effects.add(metadata['command'] as String);
            case (ART_DOMAIN_UI, ART_UI_TRANSLATE):
              require(
                metadata['text'] == 'original text',
                'Unexpected translation: $metadata',
              );
              require(
                result.flags & ART_WAIT_UI == 0,
                'Translation blocked the scenario',
              );
              send(event.reply({'status': 'ok', 'text': 'translated text'}));
              effects.add('translated');
            case (ART_DOMAIN_MEDIA, ART_MEDIA_AUDIO_COMMAND):
              audioEvent = event;
            case (ART_DOMAIN_MEDIA, ART_MEDIA_VIDEO_COMMAND):
              videoEvent = event;
            default:
              require(
                !event.requiresReply,
                'Unexpected request ${event.domain}/${event.kind}',
              );
          }
        }
      }
      if (result.status != ART_WOULD_BLOCK && result.flags & ART_WAIT_IO == 0)
        return;
    }
    throw StateError('Scenario did not reach a wait');
  }

  pump(1);
  final borrowed = session.withLatestFrame(0, (frame) {
    final packet = frame.copyPacket();
    require(
      packet.length >= 4 && ascii.decode(packet.sublist(0, 4)) == 'ARFP',
      'Invalid frame packet',
    );
    require(
      frame.copyTextureRgba(0).join(',') == '17,34,51,255',
      'Unexpected RGBA texture',
    );
    send(
      ArtemisMessage(
        domain: ART_DOMAIN_INPUT,
        kind: ART_INPUT_WINDOW_BUTTON,
        epoch: epoch,
        metadata: {'button': 1},
      ),
    );
    pump(2);
    send(
      ArtemisMessage(
        domain: ART_DOMAIN_INPUT,
        kind: ART_INPUT_KEY,
        epoch: epoch,
        metadata: {'code': 13, 'down': true},
      ),
    );
    pump(3);
    require(
      frame.copyTextureRgba(0).join(',') == '17,34,51,255',
      'Borrowed pixels changed during step',
    );
    return frame;
  });
  require(
    effects.join(',') == 'fullscreen,large,translated,window,advanced',
    'Unexpected effects: $effects',
  );
  if (borrowed == null) throw StateError('No frame available');
  var rejectedAccess = false;
  try {
    borrowed.copyPacket();
  } on StateError {
    rejectedAccess = true;
  }
  require(rejectedAccess, 'Released frame memory can still be accessed');

  if (!mediaEnabled) {
    pump(4);
    pump(5);
    require(
      audioEvent == null && videoEvent == null && effects.last == 'media_done',
      'A Host without media capability received commands or stalled',
    );
    return;
  }
  final audio = audioEvent?.mediaCommand;
  if (audio is! ArtemisAudioPlay ||
      audio.channel != ArtemisAudioChannel.se ||
      audio.scriptId != 'fx' ||
      audio.key != 'ping.ogg' ||
      audio.gain != 450) {
    throw StateError('Unexpected audio play command');
  }
  final complete = audioEvent!.playbackStatus(
    audio.id,
    ArtemisPlaybackStatus.ended,
  );
  try {
    other.submit([complete]);
    throw StateError('A foreign session accepted playback feedback');
  } on ArgumentError {
    send(audioEvent!.playbackStatus(audio.id, ArtemisPlaybackStatus.started));
    send(complete);
  }
  pump(4);
  final video = videoEvent?.mediaCommand;
  if (video is! ArtemisVideoPlay ||
      video.layer != '1.80' ||
      BigInt.parse(video.id) <= (BigInt.one << 63)) {
    throw StateError('Unexpected layer video command or truncated playback ID');
  }
  final pixels = Uint8List.fromList([8, 16, 32, 255]);
  final frame = videoEvent!.videoFrame(1, 1, 1, pixels);
  send(frame);
  pump(5);
  require(
    session.withLatestFrame(0, (f) => f.copyTextureRgba(1).join(',')) ==
        pixels.join(','),
    'Layer video pixels did not reach the Core frame',
  );
  try {
    send(frame);
    throw StateError('Core accepted a duplicate video frame');
  } on ArtemisError catch (error) {
    if (error.status != ART_E_STALE) rethrow;
  }
  send(videoEvent!.playbackStatus(video.id, ArtemisPlaybackStatus.ended));
  pump(6);
  require(
    effects.last == 'media_done',
    'Video feedback did not resume the script',
  );
}

void exerciseChunkedWrite(
  ArtemisSession session,
  ArtemisSession other,
  Directory dir,
) {
  const prefix = 'previous:';
  const repeats = 524300;
  const total = prefix.length + 2 * repeats;
  final saved = File('${dir.path}/Sub/Slot')..createSync(recursive: true);
  saved.writeAsStringSync(prefix, flush: true);
  final protected = File('${dir.path}/blocked/keep')
    ..createSync(recursive: true);
  protected.writeAsStringSync('keep');
  final boot = utf8.encode('''
*top
[calllua function=extend]
[file command=delete target=sub/empty]
[caption data=committed]
[stop]
[lua]
function extend(e)
  local denied, reason = io.open('save/blocked', 'w')
  assert(denied == nil and string.find(reason, 'denied'))
  local f = assert(io.open('save/sub/slot', 'a'))
  f:write(string.rep('Az', $repeats))
  assert(f:close())
  assert(e:file('save/sub/slot') == '$prefix' .. string.rep('Az', $repeats))
  local empty = assert(io.open('save/sub/empty', 'w'))
  assert(empty:close() and e:isFileExists('save/sub/empty'))
  assert(e:file('save/sub/empty') == '')
end
[/lua]
''');
  final io = ArtemisFileIo(
    session: session,
    saveRoot: dir,
    readAsset: (key) =>
        key == 'boot.iet' ? (bytes: boot, modified: null) : null,
  );
  void open(ArtemisSession run) => run.openProject(
    utf8.encode('[WINDOWS]\nWIDTH=320\nHEIGHT=200\nBOOT=boot.iet\n'),
    platform: 'windows',
  );
  bool pendingExists() =>
      saved.parent.listSync(followLinks: false).any((e) => e is Directory);
  ArtemisEvent? abandoned;
  void pump({required bool abandonAfterStaging}) {
    for (var turn = 0; turn < 64; turn++) {
      session.step(
        frameId: 1,
        monotonicNs: 0,
        utcUnixMs: 0,
        utcOffsetMinutes: 0,
      );
      for (
        var events = session.pollEvents();
        events.isNotEmpty;
        events = session.pollEvents()
      ) {
        for (final event in events) {
          final reply = io.handle(event);
          if (reply != null) {
            require(session.submit([reply]), 'File IO reply did not fit');
            if (reply.metadata['status'] == 'staged') {
              require(
                saved.readAsStringSync() == prefix && pendingExists(),
                'A partial write changed the live file',
              );
              if (abandonAfterStaging) {
                abandoned = event;
                return;
              }
            }
          } else if (event.domain == ART_DOMAIN_RUNTIME &&
              event.kind == ART_RUNTIME_SESSION_CHANGED &&
              abandoned != null) {
            require(
              !pendingExists() && io.handle(abandoned!) == null,
              'Reload did not retire the old write',
            );
          } else if (event.domain == ART_DOMAIN_UI &&
              event.kind == ART_UI_CAPTION) {
            require(
              !abandonAfterStaging && event.metadata['text'] == 'committed',
              'Unexpected caption',
            );
            return;
          } else {
            require(
              !event.requiresReply,
              'Unexpected request ${event.domain}/${event.kind}',
            );
          }
        }
      }
    }
    throw StateError('File IO smoke did not finish');
  }

  try {
    open(session);
    pump(abandonAfterStaging: true);
    require(abandoned != null && pendingExists(), 'No transaction to abandon');
    do {
      open(other);
    } while (other.state.epoch <= session.state.epoch);
    try {
      io.handle(other.pollEvents().single);
      throw StateError(
        'The file service accepted an event from another session',
      );
    } on ArgumentError {
      require(pendingExists(), 'A foreign session discarded the staged file');
    }
    open(session);
    pump(abandonAfterStaging: false);
    require(
      saved.lengthSync() == total && !pendingExists(),
      'Chunked write did not complete',
    );
    require(
      !File('${saved.parent.path}/empty').existsSync(),
      'Delete was not applied',
    );
    require(
      protected.readAsStringSync() == 'keep',
      'A rejected write modified the directory',
    );
  } finally {
    io.close();
  }
}

void main(List<String> arguments) {
  if (arguments.length != 1)
    throw ArgumentError('Pass the path to the standalone Artemis library');
  final library = DynamicLibrary.open(arguments.single);
  ArtemisSession create({required bool media}) => ArtemisSession.create(
    library,
    hostCapabilities:
        ArtemisFileIo.capabilities |
        ART_HOST_CAP_TRANSLATION |
        (media ? ArtemisMediaCommand.capability : 0),
    rendererCapabilities: ART_RENDERER_CAP_SPRITE,
  );
  final session = create(media: true);
  try {
    final other = create(media: false);
    try {
      final state = session.state;
      require(
        state.phase == ART_RUNTIME_CREATED && state.epoch == 0,
        'Invalid initial state',
      );
      require(
        session.withLatestFrame(0, (frame) => frame.id) == null,
        'Unexpected initial frame',
      );
      exerciseHost(session, other, mediaEnabled: true);
      exerciseHost(other, session, mediaEnabled: false);
      final dir = Directory.systemTemp.createTempSync('artemis-v2-write-');
      try {
        exerciseChunkedWrite(session, other, dir);
      } finally {
        dir.deleteSync(recursive: true);
      }
    } finally {
      other.close();
    }
  } finally {
    session.close();
  }
  print('Artemis v2 Dart Host SDK smoke passed');
}
