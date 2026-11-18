import 'dart:io';

import 'artemis_file_service.dart';
import 'artemis_host.dart';

export 'artemis_file_service.dart' show ArtemisReadAsset;

/// Connects the file service to a direct FFI session.
final class ArtemisFileIo {
  ArtemisFileIo({
    required ArtemisSession session,
    required Directory saveRoot,
    required ArtemisReadAsset readAsset,
  }) : _session = session,
       _service = ArtemisFileService(saveRoot: saveRoot, readAsset: readAsset);

  static const capabilities = ArtemisFileService.capabilities;

  final ArtemisSession _session;
  final ArtemisFileService _service;

  ArtemisReadAsset get readAsset => _service.readAsset;

  ArtemisMessage? handle(ArtemisEvent event) {
    if (!event.belongsTo(_session)) {
      throw ArgumentError('The event belongs to another Artemis session');
    }
    final reply = _service.handle(
      ArtemisFileEvent(
        event.domain,
        event.kind,
        event.epoch,
        event.requiresReply,
        event.metadata,
        event.blob,
      ),
    );
    if (reply == null) return null;
    return event.reply(reply.metadata, blob: reply.blob ?? const []);
  }

  void close() => _service.close();
}
