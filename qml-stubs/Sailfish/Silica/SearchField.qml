import QtQuick 2.6
// Silica SearchField = TextField + search icon + clear button. Only Vuo's
// surface declared.
TextInput {
    property string label
    property string placeholderText
    property bool active: true
    property bool canHide: false
}
