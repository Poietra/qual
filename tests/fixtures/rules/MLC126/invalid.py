from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        image = ImageMobject("photo.png")
        fade = FadeIn(square)
        group = VGroup(square, image)
        group.add(3)
        group.add("label")
        pile = VGroup(ImageMobject("sprite.png"))
        family = Group(fade)
