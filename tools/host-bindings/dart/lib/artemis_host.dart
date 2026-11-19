import 'dart:convert';
import 'dart:ffi';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

import 'artemis_media.dart';
import 'artemis_v2.dart';

export 'artemis_media.dart';
export 'artemis_v2.dart';

final class ArtemisError implements Exception {
  const ArtemisError(this.operation, this.status, [this.detail = '']);

  final String operation;
  final int status;
  final String detail;

  @override
  String toString() =>
      'Artemis $operation ($status)${detail.isEmpty ? '' : ': $detail'}';
}

final class ArtemisMessage {
  const ArtemisMessage({
    required this.domain,
    required this.kind,
    required this.epoch,
    required this.metadata,
    this.requestId = 0,
    this.blob = const [],
  }) : _sessionKey = null;

  ArtemisMessage._reply(ArtemisEvent event, this.metadata, this.blob)
    : domain = event.domain,
      kind = event.kind,
      epoch = event.epoch,
      requestId = event.requestId,
      _sessionKey = event._sessionKey;

  ArtemisMessage._media(
    ArtemisEvent event,
    this.kind,
    this.metadata, [
    this.blob = const [],
  ]) : domain = ArtemisMediaCommand.domain,
       epoch = event.epoch,
       requestId = 0,
       _sessionKey = event._sessionKey;

  final int domain;
  final int kind;
  final int epoch;
  final int requestId;
  final Map<String, Object?> metadata;
  final List<int> blob;
  final Object? _sessionKey;
}

final class ArtemisEvent {
  ArtemisEvent._({
    required this.domain,
    required this.kind,
    required this.epoch,
    required this.sequence,
    required this.requestId,
    required this.metadata,
    required this.blob,
    required Object sessionKey,
  }) : _sessionKey = sessionKey;

  final int domain;
  final int kind;
  final int epoch;
  final int sequence;
  final int requestId;
  final Map<String, Object?> metadata;
  final Uint8List blob;
  final Object _sessionKey;

  bool get requiresReply => requestId != 0;

  late final mediaCommand = ArtemisMediaCommand.decode(domain, kind, metadata);

  ArtemisMessage playbackStatus(
    String playbackId,
    ArtemisPlaybackStatus status,
  ) => ArtemisMessage._media(
    this,
    ArtemisMediaCommand.statusKind,
    _media.status(playbackId, status),
  );

  ArtemisMessage videoFrame(int frame, int width, int height, Uint8List rgba) =>
      ArtemisMessage._media(
        this,
        ArtemisMediaCommand.frameKind,
        _media.videoFrame(frame, width, height, rgba),
        rgba,
      );

  ArtemisMediaCommand get _media =>
      mediaCommand ?? (throw StateError('This event is not a media command'));

  bool belongsTo(ArtemisSession session) =>
      identical(_sessionKey, session._sessionKey);

  ArtemisMessage reply(
    Map<String, Object?> metadata, {
    List<int> blob = const [],
  }) {
    if (!requiresReply) throw StateError('This event does not accept a reply');
    return ArtemisMessage._reply(this, metadata, blob);
  }
}

typedef ArtemisStep = ({
  int status,
  int flags,
  int frameId,
  int renderId,
  int pendingRequests,
});
typedef ArtemisState = ({
  int phase,
  int epoch,
  int frameId,
  int width,
  int height,
  int waitFlags,
  bool exitRequested,
  int pendingRequests,
  int controlQueueBytes,
  int droppedDiagnostics,
});

/// A caller-driven session. All calls, including [close], require the OS
/// thread that created it. The library must remain loaded until close returns.
final class ArtemisSession {
  ArtemisSession._(this._root, this._runtime, this._frames, this._handle);

  factory ArtemisSession.create(
    DynamicLibrary library, {
    required int hostCapabilities,
    required int rendererCapabilities,
    int randomSeed = 0,
  }) => using((arena) {
    final native = ArtemisV2(library);
    final outRoot = arena<Pointer<ArtRootApiV2>>();
    final status = native.art3m1s_get_api_v2(
      ART_ABI_V2_MAJOR,
      sizeOf<ArtRootApiV2>(),
      outRoot,
    );
    if (status != ART_OK) throw ArtemisError('get_api', status);
    if (outRoot.value == nullptr)
      throw const FormatException('Missing Artemis root table');
    final root = outRoot.value.ref;
    _checkHeader(root.header, 0, sizeOf<ArtRootApiV2>());
    final query = root.query_interface
        .asFunction<int Function(int, int, int, Pointer<Pointer<Void>>)>();
    final table = arena<Pointer<Void>>();
    Pointer<Void> get(int id, int size) {
      _check(root, 'query_interface', query(id, ART_ABI_V2_MAJOR, size, table));
      if (table.value == nullptr)
        throw const FormatException('Missing Artemis interface table');
      return table.value;
    }

    final runtime = get(
      ART_INTERFACE_RUNTIME,
      sizeOf<ArtRuntimeApiV2>(),
    ).cast<ArtRuntimeApiV2>().ref;
    _checkHeader(
      runtime.header,
      ART_INTERFACE_RUNTIME,
      sizeOf<ArtRuntimeApiV2>(),
    );
    final frames = get(
      ART_INTERFACE_FRAME,
      sizeOf<ArtFrameApiV2>(),
    ).cast<ArtFrameApiV2>().ref;
    _checkHeader(frames.header, ART_INTERFACE_FRAME, sizeOf<ArtFrameApiV2>());
    final config = arena<ArtCreateInfo>();
    config.ref
      ..struct_size = sizeOf<ArtCreateInfo>()
      ..host_service_caps = hostCapabilities
      ..renderer_caps = rendererCapabilities
      ..random_seed = randomSeed;
    final outHandle = arena<Uint64>();
    final create = runtime.create
        .asFunction<int Function(Pointer<ArtCreateInfo>, Pointer<Uint64>)>();
    _check(root, 'create', create(config, outHandle));
    return ArtemisSession._(root, runtime, frames, outHandle.value);
  });

  final ArtRootApiV2 _root;
  final ArtRuntimeApiV2 _runtime;
  final ArtFrameApiV2 _frames;
  final Object _sessionKey = Object();
  int _handle;

  int get _liveHandle {
    if (_handle == 0) throw StateError('Artemis session is closed');
    return _handle;
  }

  /// Destroys the session. For normal exit, wait for the closed event first.
  void close() {
    if (_handle == 0) return;
    final destroy = _runtime.destroy.asFunction<int Function(int)>();
    _check(_root, 'destroy', destroy(_handle));
    _handle = 0;
  }

  /// Requests a graceful shutdown of the running project.
  /// If this returns false, finish pending work and retry. Once accepted, keep
  /// stepping and servicing events until ART_RUNTIME_CLOSED_EVENT is received.
  bool requestShutdown() => submit([
    ArtemisMessage(
      domain: ART_DOMAIN_RUNTIME,
      kind: ART_RUNTIME_SHUTDOWN,
      epoch: state.epoch,
      metadata: const {},
    ),
  ]);

  void openProject(
    List<int> ini, {
    required String platform,
    String? reportedOs,
  }) => using((arena) {
    final open = _runtime.open_project
        .asFunction<int Function(int, Pointer<ArtProjectInfo>)>();
    final project = arena<ArtProjectInfo>();
    project.ref.struct_size = sizeOf<ArtProjectInfo>();
    _setBytes(project.ref.ini_bytes, ini, arena);
    _setBytes(project.ref.platform_utf8, utf8.encode(platform), arena);
    if (reportedOs != null)
      _setBytes(project.ref.reported_os_utf8, utf8.encode(reportedOs), arena);
    _check(_root, 'open_project', open(_liveHandle, project));
  });

  ArtemisState get state => using((arena) {
    final getState = _runtime.get_state
        .asFunction<int Function(int, Pointer<ArtRuntimeState>)>();
    final out = arena<ArtRuntimeState>();
    out.ref.struct_size = sizeOf<ArtRuntimeState>();
    _check(_root, 'get_state', getState(_liveHandle, out));
    final state = out.ref;
    return (
      phase: state.state,
      epoch: state.session_epoch,
      frameId: state.host_frame_id,
      width: state.stage_width,
      height: state.stage_height,
      waitFlags: state.wait_flags,
      exitRequested: state.exit_requested != 0,
      pendingRequests: state.pending_requests,
      controlQueueBytes: state.control_queue_bytes,
      droppedDiagnostics: state.dropped_diagnostic_records,
    );
  });

  /// With [backgroundAutosave], drive the save without advancing the game frame.
  ArtemisStep step({
    required int frameId,
    required int monotonicNs,
    required int utcUnixMs,
    required int utcOffsetMinutes,
    bool backgroundAutosave = false,
  }) => using((arena) {
    final step = _runtime.step
        .asFunction<
          int Function(int, Pointer<ArtStepInfo>, Pointer<ArtStepResult>)
        >();
    final info = arena<ArtStepInfo>();
    info.ref
      ..struct_size = sizeOf<ArtStepInfo>()
      ..flags = backgroundAutosave ? ART_STEP_BACKGROUND_AUTOSAVE : 0
      ..host_frame_id = frameId
      ..monotonic_ns = monotonicNs
      ..utc_unix_ms = utcUnixMs
      ..utc_offset_minutes = utcOffsetMinutes;
    final out = arena<ArtStepResult>();
    out.ref.struct_size = sizeOf<ArtStepResult>();
    final status = step(_liveHandle, info, out);
    if (status != ART_WOULD_BLOCK) _check(_root, 'step', status);
    return (
      status: status,
      flags: out.ref.flags,
      frameId: out.ref.host_frame_id,
      renderId: out.ref.latest_render_frame_id,
      pendingRequests: out.ref.pending_requests,
    );
  });

  /// Returns false on queue backpressure; the batch can be retried unchanged.
  bool submit(List<ArtemisMessage> batch) => using((arena) {
    final handle = _liveHandle;
    if (batch.isEmpty) return true;
    final submit = _runtime.submit
        .asFunction<int Function(int, Pointer<ArtMessage>, int)>();
    final messages = arena<ArtMessage>(batch.length);
    for (var i = 0; i < batch.length; i++) {
      final record = messages[i];
      final message = batch[i];
      if (message._sessionKey != null && message._sessionKey != _sessionKey) {
        throw ArgumentError('The message belongs to another Artemis session');
      }
      record
        ..struct_size = sizeOf<ArtMessage>()
        ..domain = message.domain
        ..schema_version = ART_SUBMIT_SCHEMA_VERSION
        ..kind = message.kind
        ..session_epoch = message.epoch
        ..request_id = message.requestId;
      _setBytes(
        record.metadata_utf8,
        utf8.encode(jsonEncode(message.metadata)),
        arena,
      );
      _setBytes(record.blob, message.blob, arena);
    }
    final status = submit(handle, messages, batch.length);
    if (status == ART_WOULD_BLOCK) return false;
    _check(_root, 'submit', status);
    return true;
  });

  /// Reads one batch. Records and blobs remain usable across steps and reloads.
  List<ArtemisEvent> pollEvents() => using((arena) {
    final handle = _liveHandle;
    final poll = _runtime.poll_events
        .asFunction<
          int Function(int, Pointer<Uint8>, int, Pointer<ArtPollResult>)
        >();
    final out = arena<ArtPollResult>();
    out.ref.struct_size = sizeOf<ArtPollResult>();
    var capacity = 4096;
    var bytes = arena<Uint8>(capacity);
    var status = poll(handle, bytes, capacity, out);
    if (status == ART_E_BUFFER) {
      final required = out.ref.required_next_bytes;
      if (required <= capacity)
        throw const FormatException('Invalid Artemis event buffer size');
      capacity = required;
      bytes = arena<Uint8>(capacity);
      status = poll(handle, bytes, capacity, out);
    }
    if (status == ART_NO_DATA) return const [];
    _check(_root, 'poll_events', status);
    if (out.ref.written_bytes < 0 || out.ref.written_bytes > capacity) {
      throw const FormatException('Invalid Artemis event batch size');
    }
    return _decodeEvents(
      bytes.asTypedList(out.ref.written_bytes),
      out.ref.record_count,
      handle,
      _sessionKey,
    );
  });

  /// The callback runs synchronously. Copy any bytes needed after it returns.
  T? withLatestFrame<T>(int after, T Function(ArtemisFrame) visit) =>
      using((arena) {
        final handle = _liveHandle;
        final acquire = _frames.acquire_latest
            .asFunction<int Function(int, int, Pointer<ArtFrameView>)>();
        final release = _frames.release.asFunction<int Function(int, int)>();
        final out = arena<ArtFrameView>();
        out.ref.struct_size = sizeOf<ArtFrameView>();
        final status = acquire(handle, after, out);
        if (status == ART_NO_DATA) return null;
        _check(_root, 'acquire_latest', status);
        final view = ArtemisFrame._(this, handle, out.ref);
        try {
          return visit(view);
        } finally {
          view._active = false;
          _check(_root, 'release', release(handle, view._token));
        }
      });
}

final class ArtemisFrame {
  ArtemisFrame._(this._session, this._handle, ArtFrameView view)
    : id = view.render_frame_id,
      epoch = view.session_epoch,
      schemaVersion = view.schema_version,
      _token = view.token,
      _packet = view.packet;

  final int id;
  final int epoch;
  final int schemaVersion;
  final ArtemisSession _session;
  final int _handle;
  final int _token;
  final ArtBytes _packet;
  bool _active = true;

  Uint8List copyPacket() {
    _checkActive();
    return _copyBytes(_packet);
  }

  Uint8List copyTextureRgba(int index) {
    _checkActive();
    if (index < 0 || index > 0xffffffff)
      throw RangeError.range(index, 0, 0xffffffff);
    return using((arena) {
      final texture = _session._frames.texture_rgba
          .asFunction<int Function(int, int, int, Pointer<ArtBytes>)>();
      final out = arena<ArtBytes>();
      _check(
        _session._root,
        'texture_rgba',
        texture(_handle, _token, index, out),
      );
      return _copyBytes(out.ref);
    });
  }

  void _checkActive() {
    if (!_active) throw StateError('Artemis frame lease has ended');
  }
}

void _checkHeader(ArtApiHeader header, int id, int size) {
  if (header.magic != ART_API_MAGIC ||
      header.abi_major != ART_ABI_V2_MAJOR ||
      header.interface_id != id ||
      header.struct_size < size) {
    throw const FormatException('Invalid Artemis API table header');
  }
}

void _check(ArtRootApiV2 root, String operation, int status) {
  if (status == ART_OK) return;
  final detail = using((arena) {
    final copy = root.copy_last_error
        .asFunction<int Function(Pointer<Uint8>, int, Pointer<Uint64>)>();
    final needed = arena<Uint64>();
    if (copy(nullptr, 0, needed) != ART_E_BUFFER || needed.value <= 0)
      return '';
    final buffer = arena<Uint8>(needed.value);
    if (copy(buffer, needed.value, needed) != ART_OK) return '';
    return utf8.decode(buffer.asTypedList(needed.value), allowMalformed: true);
  });
  throw ArtemisError(operation, status, detail);
}

void _setBytes(ArtBytes out, List<int> bytes, Arena arena) {
  if (bytes.isEmpty) return;
  final buffer = arena<Uint8>(bytes.length);
  buffer.asTypedList(bytes.length).setAll(0, bytes);
  out.data = buffer;
  out.size = bytes.length;
}

Uint8List _copyBytes(ArtBytes bytes) => bytes.size == 0
    ? Uint8List(0)
    : Uint8List.fromList(bytes.data.asTypedList(bytes.size));

List<ArtemisEvent> _decodeEvents(
  Uint8List bytes,
  int count,
  int handle,
  Object sessionKey,
) {
  final data = ByteData.sublistView(bytes);
  final events = <ArtemisEvent>[];
  var offset = 0;
  while (offset < bytes.length) {
    const headerBytes = 64;
    const le = Endian.little;
    if (bytes.length - offset < headerBytes)
      throw const FormatException('Truncated Artemis event');
    final size = data.getUint32(offset, le);
    final metadataSize = data.getUint32(offset + 56, le);
    final blobSize = data.getUint32(offset + 60, le);
    if (size != headerBytes + metadataSize + blobSize ||
        size > bytes.length - offset ||
        data.getUint16(offset + 4, le) != 2 ||
        data.getUint64(offset + 16, le) != handle ||
        data.getUint32(offset + 48, le) != size - 56) {
      throw const FormatException('Invalid Artemis event envelope');
    }
    final metadataStart = offset + headerBytes;
    final blobStart = metadataStart + metadataSize;
    final metadata = jsonDecode(
      utf8.decode(Uint8List.sublistView(bytes, metadataStart, blobStart)),
    );
    if (metadata is! Map<String, Object?>)
      throw const FormatException('Invalid Artemis event metadata');
    events.add(
      ArtemisEvent._(
        domain: data.getUint16(offset + 6, le),
        kind: data.getUint32(offset + 8, le),
        epoch: data.getUint64(offset + 24, le),
        sequence: data.getUint64(offset + 32, le),
        requestId: data.getUint64(offset + 40, le),
        metadata: metadata,
        blob: bytes.sublist(blobStart, offset + size),
        sessionKey: sessionKey,
      ),
    );
    offset += size;
  }
  if (events.length != count)
    throw const FormatException('Invalid Artemis event count');
  return events;
}
