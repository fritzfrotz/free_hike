import Capacitor
import UIKit

/// App-local Capacitor plugins must be registered on the bridge before the
/// WebView loads. Main.storyboard instantiates this subclass in place of the
/// stock CAPBridgeViewController.
class MainViewController: CAPBridgeViewController {
    override open func capacitorDidLoad() {
        bridge?.registerPluginInstance(MapCompilerPlugin())
    }
}
