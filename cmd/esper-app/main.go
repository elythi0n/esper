// Command esper-app is the Esper window.
//
// # What it is
//
// A webview around the same bundle `/design` renders, a proxy in front of
// the local API, and — when nothing else is — the control plane itself. That
// last part is the difference between an application and a front-end: open
// it and it works, with no second thing to start in a terminal.
//
// It holds no product logic. Everything a screen shows comes from the
// control plane over HTTP, which is the seam the MCP adapter and
// `esper doctor` already use. A window that grew its own way of asking would
// be a third answer to every question.
//
// # Two jobs worth reading about
//
// Reading the API token and never letting it reach the page (proxy.go), and
// opening a second window for a popped-out session, because the webview
// cannot open one for itself (shellapi.go).
package main

import (
	"context"
	"embed"
	"errors"
	"flag"
	"fmt"
	"io/fs"
	"net/http"
	"os"
	"os/signal"
	"strings"
	"syscall"

	"github.com/elythi0n/esper/internal/home"
	"github.com/elythi0n/esper/internal/proto"
	"github.com/elythi0n/esper/internal/server"
	"github.com/wailsapp/wails/v2"
	"github.com/wailsapp/wails/v2/pkg/options"
	"github.com/wailsapp/wails/v2/pkg/options/assetserver"
	"github.com/wailsapp/wails/v2/pkg/options/linux"
)

// The built web bundle, embedded so the window is one file to ship.
//
// `all:` because the bundle has directories Go would otherwise skip. A
// placeholder index.html is committed so a checkout that has never built the
// web compiles and says so on screen rather than opening blank.
//
//go:embed all:dist
var bundle embed.FS

func main() {
	if code := run(); code != 0 {
		os.Exit(code)
	}
}

func run() int {
	// A secondary window: a session popped out of the main one. It takes no
	// lock, starts no control plane, and simply shows a route.
	windowRoute := flag.String("window", "", "open a second window on this route instead of the main one")
	// **The bundle is fingerprinted, not stamped.** The window carries the
	// interface inside itself, built by a different toolchain, so a rebuilt
	// binary holding yesterday's `dist` looks exactly like a fix that did not
	// work. A hash of what is actually embedded cannot be wrong about that.
	showVersion := flag.Bool("version", false, "print the version, the protocol, and a fingerprint of the embedded interface")
	flag.Parse()
	if *showVersion {
		fmt.Printf("esper-app %s\n", version)
		fmt.Printf("  protocol  %d\n", proto.ProtocolVersion)
		fingerprint, err := bundleFingerprint()
		if err != nil {
			fmt.Printf("  interface  could not be read: %v\n", err)
			return 1
		}
		fmt.Printf("  interface %s (%d files)\n", fingerprint, bundleFiles())
		return 0
	}
	secondary := *windowRoute != ""

	// **A link is an argument, because that is how the OS hands one over.**
	// The desktop entry registers `x-scheme-handler/esper` and is launched
	// with the URL, so an invitation arrives as argv rather than through any
	// mechanism of ours.
	opening := ""
	if rest := flag.Args(); len(rest) > 0 && looksLikeALink(rest[0]) {
		opening = routeForLink(rest[0])
		if opening == "" {
			fmt.Fprintf(os.Stderr, "esper-app: %q is not an Esper invitation\n", rest[0])
			return 1
		}
	}

	// The handle the window's runtime is stored on, made before the lock
	// because a second launch can hand this one a link the moment it binds.
	win := &theWindow{}

	esperHome, err := home.Dir()
	if err != nil {
		fmt.Fprintf(os.Stderr, "esper-app: %v\n", err)
		return 1
	}

	if !secondary {
		// **One main window.** A second launch — from a tray, a desktop file
		// or a deep link — should raise the one that is open rather than
		// start a second webview holding the same token.
		lock, lockErr := single(esperHome)
		switch {
		case errors.Is(lockErr, errAlreadyOpen):
			// Raise the window that is open, on the link if there was one.
			// Starting a second webview holding the same token would be the
			// wrong answer to every launch, and to a deep link especially.
			if handErr := handTo(esperHome, opening); handErr != nil {
				fmt.Fprintf(os.Stderr, "esper-app: an Esper window is already open, and it did not answer: %v\n", handErr)
			} else {
				fmt.Fprintln(os.Stderr, "esper-app: an Esper window is already open")
			}
			return 0
		case lockErr != nil:
			fmt.Fprintf(os.Stderr, "esper-app: %v\n", lockErr)
			return 1
		}
		defer lock.release()
		go lock.accept(func(route string) { win.show(route) })
	}

	// The token, read here and nowhere else.
	token, err := server.LoadOrCreateToken(esperHome)
	if err != nil {
		fmt.Fprintf(os.Stderr, "esper-app: %v\n", err)
		return 1
	}

	addr := os.Getenv("ESPER_API_ADDR")
	if addr == "" {
		addr = server.DefaultAddr
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	if !secondary {
		if _, planeErr := runControlPlane(ctx, esperHome, token, addr); planeErr != nil {
			// Not fatal. The window opens on its first-run screen, which says
			// what is wrong and retries — better than refusing to start.
			fmt.Fprintf(os.Stderr, "esper-app: %v\n", planeErr)
		}
	}

	proxy, err := proxyToAPI(addr, token)
	if err != nil {
		fmt.Fprintf(os.Stderr, "esper-app: %v\n", err)
		return 1
	}
	// Where a session writes its pictures. One socket per window: a
	// popped-out session is a window of its own and gets its own, so two
	// sessions on screen are two streams rather than one being stolen.
	//
	// Not fatal when it cannot be opened — a window that cannot show a
	// picture is still a window that pairs devices, shows what has happened
	// and manages this machine, which is all it could do yesterday.
	pictures, viewerErr := openViewer(esperHome, secondary)
	if viewerErr != nil {
		fmt.Fprintf(os.Stderr, "esper-app: no socket for a session's pictures: %v\n", viewerErr)
	} else {
		defer pictures.close()
	}

	// Only the main window can open another; a popped-out one is a leaf.
	shell := shellAPI(!secondary, win, pictures)

	assets, err := fs.Sub(bundle, "dist")
	if err != nil {
		fmt.Fprintf(os.Stderr, "esper-app: the bundle is not where it should be: %v\n", err)
		return 1
	}

	title := "Esper"
	if secondary {
		title = "Esper — session"
	}

	// Where this window opens: a popped-out session's route, or the link that
	// launched it, or nowhere in particular.
	startRoute := *windowRoute
	if startRoute == "" {
		startRoute = opening
	}

	// **The main window remembers its shape; a popped-out one does not.**
	// A session window is opened for one session and closed with it, and
	// two windows writing one file would each keep undoing the other.
	shape := windowState{Width: 1280, Height: 840}
	if !secondary {
		if remembered := loadWindowState(esperHome); remembered.sane() {
			shape = remembered
		}
	}

	err = wails.Run(&options.App{
		Title:     title,
		Width:     shape.Width,
		Height:    shape.Height,
		MinWidth:  480,
		MinHeight: 400,
		// **No system chrome.** The bar is the product's, so the window is
		// frameless and the page draws its own — see window.go for the
		// buttons that then become ours to provide.
		Frameless: true,
		OnStartup: func(ctx context.Context) {
			win.set(ctx)
			if secondary {
				return
			}
			restoreWindow(ctx, shape)
			go watchWindowState(ctx, esperHome, shape)
		},
		// The last word on the shape, so a window closed from its own button
		// is remembered even between ticks of the watch above.
		OnBeforeClose: func(ctx context.Context) bool {
			if !secondary {
				saveWindowState(esperHome, currentWindowState(ctx))
			}
			return false
		},
		AssetServer: &assetserver.Options{
			Assets: assets,
			Middleware: func(next http.Handler) http.Handler {
				return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
					switch {
					case strings.HasPrefix(r.URL.Path, apiPrefix):
						proxy.ServeHTTP(w, r)
					case strings.HasPrefix(r.URL.Path, shellPrefix):
						shell.ServeHTTP(w, r)
					default:
						// The document is rewritten on the way out: it is
						// where the page learns that its window is frameless,
						// and — for a popped-out one — which session it is.
						if isDocument(r.URL.Path) {
							serveDocument(w, r, next, startRoute, addr)
							return
						}
						next.ServeHTTP(w, r)
					}
				})
			},
		},
		// The palette the bundle paints over, so the first frame is not white.
		// `--n-0` from the token file, which is the one place it is decided.
		BackgroundColour: &options.RGBA{R: 0x0B, G: 0x0B, B: 0x0D, A: 1},
		Linux: &linux.Options{
			ProgramName:         "Esper",
			WindowIsTranslucent: false,
		},
	})
	if err != nil {
		fmt.Fprintf(os.Stderr, "esper-app: %v\n", err)
		return 1
	}
	return 0
}
