from manim import *


class CaseScene(Scene):
    def construct(self):
        # On-disk file is assets/icon.svg via the appended .svg extension:
        # MLR104's literal-level scan cannot map this back onto the
        # literal, so the case cause is this rule's finding.
        icon = SVGMobject("ICON")
        # On-disk file is assets/picture.png via the raster extensions.
        photo = ImageMobject("Picture")
        self.add(icon, photo)
