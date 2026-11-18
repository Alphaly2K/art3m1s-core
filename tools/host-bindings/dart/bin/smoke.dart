import 'dart:convert';
import 'dart:ffi';

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

void main(List<String> arguments) {
  if (arguments.length != 1)
    throw ArgumentError('Pass the path to the standalone Artemis library');
  final library = DynamicLibrary.open(arguments.single);
  ArtemisSession create() => ArtemisSession.create(
    library,
    hostCapabilities: ART_HOST_CAP_IO | ART_HOST_CAP_TRANSLATION,
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
    } finally {
      other.close();
    }
  } finally {
    session.close();
  }
  print('Artemis v2 Dart Host SDK smoke passed');
}
