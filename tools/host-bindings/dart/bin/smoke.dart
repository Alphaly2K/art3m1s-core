import 'dart:convert';
import 'dart:ffi';
import 'dart:typed_data';

import 'package:artemis_host_v2/artemis_v2.dart';
import 'package:ffi/ffi.dart';

void expectStatus(int actual, int expected) {
  if (actual != expected) {
    throw StateError('Expected status $expected, got $actual');
  }
}

void setBytes(ArtBytes target, List<int> bytes, Arena arena) {
  if (bytes.isEmpty) return;
  final buffer = arena<Uint8>(bytes.length);
  buffer.asTypedList(bytes.length).setAll(0, bytes);
  target.data = buffer;
  target.size = bytes.length;
}

void exerciseHost(ArtRuntimeApiV2 runtime, ArtFrameApiV2 frames, int handle) {
  const ini = '[WINDOWS]\nWIDTH=320\nHEIGHT=200\nBOOT=boot.iet\n';
  final boot = utf8.encode(
    '*top\n[lyc id=1 width=2 height=1 color=ff112233]\n'
    '[setonwindowbutton button=1 label=window call=1]\n'
    '[exec command=fullscreen]\noriginal text\n[wt0]\n[caption data=advanced]\n[stop]\n'
    '*window\n[caption data=window]\n[return]\n',
  );
  final open = runtime.open_project
      .asFunction<int Function(int, Pointer<ArtProjectInfo>)>();
  final step = runtime.step
      .asFunction<
        int Function(int, Pointer<ArtStepInfo>, Pointer<ArtStepResult>)
      >();
  final poll = runtime.poll_events
      .asFunction<
        int Function(int, Pointer<Uint8>, int, Pointer<ArtPollResult>)
      >();
  final submit = runtime.submit
      .asFunction<int Function(int, Pointer<ArtMessage>, int)>();
  using((arena) {
    final project = arena<ArtProjectInfo>();
    project.ref.struct_size = sizeOf<ArtProjectInfo>();
    setBytes(project.ref.ini_bytes, utf8.encode(ini), arena);
    setBytes(project.ref.platform_utf8, utf8.encode('windows'), arena);
    expectStatus(open(handle, project), ART_OK);
  });

  void send(
    int domain,
    int kind,
    int epoch,
    int request,
    Map<String, Object?> metadata, [
    List<int> payload = const [],
  ]) {
    using((arena) {
      final message = arena<ArtMessage>();
      message.ref
        ..struct_size = sizeOf<ArtMessage>()
        ..domain = domain
        ..schema_version = ART_SUBMIT_SCHEMA_VERSION
        ..kind = kind
        ..session_epoch = epoch
        ..request_id = request;
      setBytes(
        message.ref.metadata_utf8,
        utf8.encode(jsonEncode(metadata)),
        arena,
      );
      setBytes(message.ref.blob, payload, arena);
      expectStatus(submit(handle, message, 1), ART_OK);
    });
  }

  int epoch = 0;
  final effects = <String>[];
  Uint8List readRecord() => using((arena) {
    final result = arena<ArtPollResult>();
    result.ref.struct_size = sizeOf<ArtPollResult>();
    final status = poll(handle, nullptr, 0, result);
    if (status == ART_NO_DATA) return Uint8List(0);
    expectStatus(status, ART_E_BUFFER);
    final capacity = result.ref.required_next_bytes;
    final buffer = arena<Uint8>(capacity);
    expectStatus(poll(handle, buffer, capacity, result), ART_OK);
    if (result.ref.record_count != 1) throw StateError('Expected one record');
    return Uint8List.fromList(buffer.asTypedList(result.ref.written_bytes));
  });

  void pump(int frame) => using((arena) {
    final info = arena<ArtStepInfo>();
    info.ref
      ..struct_size = sizeOf<ArtStepInfo>()
      ..host_frame_id = frame
      ..monotonic_ns = frame * 1000000;
    final result = arena<ArtStepResult>();
    result.ref.struct_size = sizeOf<ArtStepResult>();
    for (var turn = 0; turn < 64; turn++) {
      expectStatus(step(handle, info, result), ART_OK);
      while (true) {
        final data = readRecord();
        if (data.isEmpty) break;
        final bytes = ByteData.sublistView(data);
        final size = bytes.getUint32(0, Endian.little);
        if (size != data.length || size < 64)
          throw StateError('Invalid record');
        final domain = bytes.getUint16(6, Endian.little);
        final kind = bytes.getUint32(8, Endian.little);
        epoch = bytes.getUint64(24, Endian.little);
        final request = bytes.getUint64(40, Endian.little);
        final metadataSize = bytes.getUint32(56, Endian.little);
        final metadata =
            jsonDecode(utf8.decode(data.sublist(64, 64 + metadataSize)))
                as Map<String, Object?>;
        if (domain == ART_DOMAIN_IO && kind == ART_IO_STAT) {
          final found = metadata['key'] == 'boot.iet';
          send(domain, kind, epoch, request, {
            'status': 'ok',
            'found': found,
            if (found) 'size': '${boot.length}',
            if (found) 'revision': '1',
          });
        } else if (domain == ART_DOMAIN_IO && kind == ART_IO_READ) {
          if (metadata['key'] != 'boot.iet' || metadata['offset'] != '0') {
            throw StateError('Unexpected asset read: $metadata');
          }
          send(domain, kind, epoch, request, {
            'status': 'ok',
            'revision': '1',
            'eof': true,
          }, boot);
        } else if (domain == ART_DOMAIN_UI && kind == ART_UI_CAPTION) {
          effects.add(metadata['text'] as String);
        } else if (domain == ART_DOMAIN_UI && kind == ART_UI_WINDOW_ACTION) {
          effects.add(metadata['command'] as String);
        } else if (domain == ART_DOMAIN_UI && kind == ART_UI_TRANSLATE) {
          if (metadata['text'] != 'original text' || request == 0) {
            throw StateError('Unexpected translation request: $metadata');
          }
          if ((result.ref.flags & ART_WAIT_UI) != 0) {
            throw StateError('Translation blocked the scenario');
          }
          send(domain, kind, epoch, request, {
            'status': 'ok',
            'text': 'translated text',
          });
          effects.add('translated');
        } else if (request != 0) {
          throw StateError('Unexpected request $domain/$kind');
        }
      }
      if ((result.ref.flags & ART_WAIT_IO) == 0) return;
    }
    throw StateError('Scenario did not reach a wait');
  });

  pump(1);
  using((arena) {
    final frame = arena<ArtFrameView>();
    frame.ref.struct_size = sizeOf<ArtFrameView>();
    final acquire = frames.acquire_latest
        .asFunction<int Function(int, int, Pointer<ArtFrameView>)>();
    final texture = frames.texture_rgba
        .asFunction<int Function(int, int, int, Pointer<ArtBytes>)>();
    final release = frames.release.asFunction<int Function(int, int)>();
    expectStatus(acquire(handle, 0, frame), ART_OK);
    final token = frame.ref.token;
    try {
      final packet = frame.ref.packet;
      if (packet.size < 4 ||
          ascii.decode(packet.data.asTypedList(4)) != 'ARFP') {
        throw StateError('Invalid frame packet');
      }
      final pixels = arena<ArtBytes>();
      expectStatus(texture(handle, token, 0, pixels), ART_OK);
      if (pixels.ref.size != 4 ||
          pixels.ref.data.asTypedList(4).join(',') != '17,34,51,255') {
        throw StateError('Unexpected RGBA texture');
      }
      send(ART_DOMAIN_INPUT, ART_INPUT_WINDOW_BUTTON, epoch, 0, {'button': 1});
      pump(2);
      send(ART_DOMAIN_INPUT, ART_INPUT_KEY, epoch, 0, {
        'code': 13,
        'down': true,
      });
      pump(3);
      if (effects.join(',') != 'fullscreen,translated,window,advanced') {
        throw StateError('Unexpected Host effects: $effects');
      }
    } finally {
      expectStatus(release(handle, token), ART_OK);
    }
  });
}

void main(List<String> arguments) {
  if (arguments.length != 1) {
    throw ArgumentError('Pass the path to the standalone Artemis library');
  }
  final native = ArtemisV2(DynamicLibrary.open(arguments.single));
  using((arena) {
    final root = arena<Pointer<ArtRootApiV2>>();
    expectStatus(
      native.art3m1s_get_api_v2(ART_ABI_V2_MAJOR, sizeOf<ArtRootApiV2>(), root),
      ART_OK,
    );
    if (root.value.ref.header.magic != ART_API_MAGIC) {
      throw StateError('Invalid root table header');
    }
    final query = root.value.ref.query_interface
        .asFunction<int Function(int, int, int, Pointer<Pointer<Void>>)>();
    final table = arena<Pointer<Void>>();
    expectStatus(
      query(
        ART_INTERFACE_RUNTIME,
        ART_ABI_V2_MAJOR,
        sizeOf<ArtRuntimeApiV2>(),
        table,
      ),
      ART_OK,
    );
    final runtime = table.value.cast<ArtRuntimeApiV2>().ref;
    expectStatus(
      query(
        ART_INTERFACE_FRAME,
        ART_ABI_V2_MAJOR,
        sizeOf<ArtFrameApiV2>(),
        table,
      ),
      ART_OK,
    );
    final frames = table.value.cast<ArtFrameApiV2>().ref;
    final create = runtime.create
        .asFunction<int Function(Pointer<ArtCreateInfo>, Pointer<Uint64>)>();
    final destroy = runtime.destroy.asFunction<int Function(int)>();
    final config = arena<ArtCreateInfo>();
    config.ref.struct_size = sizeOf<ArtCreateInfo>();
    config.ref.host_service_caps = ART_HOST_CAP_IO | ART_HOST_CAP_TRANSLATION;
    config.ref.renderer_caps = ART_RENDERER_CAP_SPRITE;
    final handle = arena<Uint64>();
    expectStatus(create(config, handle), ART_OK);
    try {
      final state = arena<ArtRuntimeState>();
      state.ref.struct_size = sizeOf<ArtRuntimeState>();
      final getState = runtime.get_state
          .asFunction<int Function(int, Pointer<ArtRuntimeState>)>();
      expectStatus(getState(handle.value, state), ART_OK);
      if (state.ref.state != ART_RUNTIME_CREATED ||
          state.ref.session_epoch != 0) {
        throw StateError('Invalid initial runtime state');
      }

      final view = arena<ArtFrameView>();
      view.ref.struct_size = sizeOf<ArtFrameView>();
      final acquire = frames.acquire_latest
          .asFunction<int Function(int, int, Pointer<ArtFrameView>)>();
      expectStatus(acquire(handle.value, 0, view), ART_NO_DATA);
      exerciseHost(runtime, frames, handle.value);
    } finally {
      expectStatus(destroy(handle.value), ART_OK);
    }
  });
  print('Artemis v2 Dart smoke passed');
}
