import 'dart:convert';
import 'dart:ffi';
import 'dart:io';

import 'package:artemis_host_v2/artemis_file_io.dart';
import 'package:artemis_host_v2/artemis_host.dart';

void require(bool condition, String message) {
  if (!condition) throw StateError(message);
}

void exerciseHost(ArtemisSession session, ArtemisSession other) {
  const ini = '[WINDOWS]\nWIDTH=320\nHEIGHT=200\nBOOT=boot.iet\n';
  final boot = utf8.encode(
    '*top\n[lyc id=1 width=2 height=1 color=ff112233]\n'
    '[setonwindowbutton button=1 label=window call=1]\n'
    '[exec command=fullscreen]\n[caption data=${'x' * 5000}]\n'
    'original text\n[wt0]\n[caption data=advanced]\n[stop]\n'
    '*window\n[caption data=window]\n[return]\n',
  );
  session.openProject(utf8.encode(ini), platform: 'windows');

  void send(ArtemisMessage message) {
    require(session.submit([message]), 'The smoke batch should fit');
  }

  final epoch = session.state.epoch;
  final effects = <String>[];
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
  ArtemisSession create() => ArtemisSession.create(
    library,
    hostCapabilities: ArtemisFileIo.capabilities | ART_HOST_CAP_TRANSLATION,
    rendererCapabilities: ART_RENDERER_CAP_SPRITE,
  );
  final session = create();
  try {
    final other = create();
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
      exerciseHost(session, other);
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
