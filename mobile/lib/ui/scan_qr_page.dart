import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';

import '../core/qr_payload.dart';

/// Full-screen camera that reads the desktop LAN QR and returns the payload.
class ScanQrPage extends StatefulWidget {
  const ScanQrPage({super.key});

  @override
  State<ScanQrPage> createState() => _ScanQrPageState();
}

class _ScanQrPageState extends State<ScanQrPage> {
  bool _handled = false;
  String? _hint;

  void _onDetect(BarcodeCapture capture) {
    if (_handled) {
      return;
    }
    for (final barcode in capture.barcodes) {
      final input = qrPayloadToServerInput(barcode.rawValue);
      if (input == null) {
        continue;
      }
      _handled = true;
      Navigator.of(context).pop(input);
      return;
    }
    if (capture.barcodes.isNotEmpty && mounted) {
      setState(() => _hint = '不是电脑访问二维码，请对准桌面上的二维码');
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('扫描电脑二维码')),
      body: Stack(
        fit: StackFit.expand,
        children: [
          MobileScanner(
            key: const Key('qr-scanner'),
            onDetect: _onDetect,
          ),
          Align(
            alignment: Alignment.bottomCenter,
            child: Padding(
              padding: const EdgeInsets.all(24),
              child: Text(
                _hint ?? '对准桌面「手机访问」二维码',
                textAlign: TextAlign.center,
                style: const TextStyle(
                  color: Colors.white,
                  fontSize: 16,
                  shadows: [Shadow(blurRadius: 8, color: Colors.black)],
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}
