pragma Singleton
import QtQuick 2.6

// Silica date/number formatter singleton. Only `formatDate` stubbed; all Vuo
// uses.
QtObject {
    function formatDate(date, format) { return "" }
    function formatFileSize(bytes, precision) { return "" }
}
