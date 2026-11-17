import 'dart:ffi';

import 'package:artemis_host_v2/artemis_v2.dart';
import 'package:ffi/ffi.dart';

void expectStatus(int actual, int expected) {
  if (actual != expected) {
    throw StateError('Expected status $expected, got $actual');
  }
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
    config.ref.host_service_caps = ART_HOST_CAP_IO;
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
    } finally {
      expectStatus(destroy(handle.value), ART_OK);
    }
  });
  print('Artemis v2 Dart smoke passed');
}
