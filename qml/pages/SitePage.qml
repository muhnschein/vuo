import QtQuick 2.6
import Sailfish.Silica 1.0
import Sailfish.WebView 1.0

/*
 * The article's own web page, one swipe to the right of the article.
 *
 * ArticlePage attaches this whenever the entry has a link, so the reader can
 * flick sideways to the site itself -- for whatever the feed left out, or for
 * the comments -- and flick back. It is the platform's own idiom for "there
 * is more to the right": the forward indicator appears on the article, and
 * the site page needs no button to reach and none to leave.
 *
 * NOTHING IS FETCHED UNTIL THE READER ARRIVES. Attaching a page instantiates
 * it, and a WebView that started loading then would contact the article's
 * host -- with the device's IP and the time of reading -- for every article
 * merely opened, which is the tracking §9.3 keeps off the phone. So the
 * WebView is behind a Loader that only activates once this page has actually
 * been shown, and the article's body (proxied, consent-gated) stays the
 * default. It also keeps the Gecko engine, which is heavy to bring up, out of
 * the path of opening an article.
 *
 * The page draws none of the site's own words: the header is a fixed string,
 * and the site's title -- foreign text -- is not shown (§9.3).
 */
WebViewPage {
    id: page

    /// The article's link. http(s) only: validated by the core before it was
    /// stored, and the same value "Open in browser" hands to the system.
    property string url: ""

    /// True once the reader has been here. Set on Active, not Activating,
    /// so a sideways flick that is started and abandoned fetches nothing.
    property bool _visited: false

    allowedOrientations: Orientation.All

    onStatusChanged: if (status === PageStatus.Active) {
        page._visited = true
    }

    // Until the engine is up, the same header the loaded page carries, so
    // the page has a name during the swipe rather than arriving blank.
    PageHeader {
        visible: !site.active
        title: qsTr("Website")
    }

    Loader {
        id: site
        anchors.fill: parent
        active: page._visited && page.url.length > 0

        sourceComponent: WebViewFlickable {
            id: flickable

            header: PageHeader { title: qsTr("Website") }

            // Set once rather than bound: the link is fixed for the life of
            // the page, and this is the first moment the WebView exists.
            Component.onCompleted: flickable.webView.url = page.url

            PullDownMenu {
                MenuItem {
                    // Reached by following links inside the site; the page
                    // swipe is the platform's back and must stay that.
                    text: qsTr("Back")
                    visible: flickable.webView.canGoBack
                    onClicked: flickable.webView.goBack()
                }
                MenuItem {
                    text: qsTr("Open in browser")
                    onClicked: Qt.openUrlExternally(page.url)
                }
            }

            // A thin line under the header that grows with the load, so a
            // slow site is seen to be slow rather than broken. The WebView
            // draws its own spinner in the middle as well.
            Rectangle {
                anchors.top: flickable.headerItem ? flickable.headerItem.bottom : flickable.top
                anchors.left: flickable.left
                height: Theme.paddingSmall / 2
                width: flickable.width * flickable.webView.loadProgress / 100
                visible: flickable.webView.loading
                color: Theme.highlightColor
            }
        }
    }
}
