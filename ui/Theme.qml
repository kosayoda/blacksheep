pragma Singleton

import QtQuick

QtObject {
    property color light_gray: "#fbf9ff";
    property color light_red: "#FF7F87";
    property color light_orange: "#EC936F";
    property color light_yellow: "#F4C582";
    property color light_green: "#9DB390";
    property color light_cyan: "#75B8BC";
    property color light_blue: "#7AADE9";
    property color light_purple: "#B698E2";
    property color light_magenta: "#C98FC4";

    property color dark_gray: "#26272B";
    property color dark_red: "#BA4D68";
    property color dark_orange: "#B2644F";
    property color dark_yellow: "#A27338";
    property color dark_green: "#718565";
    property color dark_cyan: "#49858A";
    property color dark_blue: "#4E7DAE";
    property color dark_purple: "#826DAF";
    property color dark_magenta: "#96638C";

    property font text_font: ({
        family: "Inter Variable Fixed",
        pointSize: 10.5,
    })

    property font icon_font: ({
        family: "Material Design Icons",
        pointSize: 12,
    })
}
