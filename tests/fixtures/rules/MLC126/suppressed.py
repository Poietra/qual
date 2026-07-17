from manim import *


class Demo(Scene):
    def construct(self):
        image = ImageMobject("photo.png")
        group = VGroup(image)  # manim-lint: ignore[MLC126]
