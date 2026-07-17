from manim import *


class Good(Scene):
    def construct(self, name):
        direct = SVGMobject("logo.svg")
        by_extension = ImageMobject("picture")
        exact = ImageMobject("picture.png")
        dynamic = SVGMobject(name)
        formatted = SVGMobject(f"{name}.svg")
        home = SVGMobject("~/somewhere.svg")
        foreign = SVGMobject("C:\\art\\logo.svg")
