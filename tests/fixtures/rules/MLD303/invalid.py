from manim import *


class PathScene(Scene):
    def construct(self):
        logo = SVGMobject("C:\\assets\\logo.svg")
        icon = SVGMobject(r"C:\icons\icon.svg")
        photo = ImageMobject("D:/pictures/photo.png")
        art = SVGMobject("art\\shape.svg")
        self.add(logo, icon, photo, art)
