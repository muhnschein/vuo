import QtQuick 2.6
// Silica's SearchField is a TextField with a search icon and a clear button.
// Only the surface Vuo uses is declared.
TextInput {
    property string label
    property string placeholderText
    property bool active: true
    property bool canHide: false
}
