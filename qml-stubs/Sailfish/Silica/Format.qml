pragma Singleton
import QtQuick 2.6

// Silica's date/number formatter singleton. Only `formatDate` is stubbed,
// because that is all Vuo uses.
QtObject {
    function formatDate(date, format) { return "" }
    function formatFileSize(bytes, precision) { return "" }
}
